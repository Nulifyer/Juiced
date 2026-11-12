mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server};

const SOURCE: &str = "Demo-7B-GGUF-Q4_K_M";

fn gguf(architecture: &str, training_context: u32, shaped: bool) -> Vec<u8> {
    fn string(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend((text.len() as u64).to_le_bytes());
        bytes.extend(text.as_bytes());
    }
    let shape: &[(&str, u32)] = if shaped {
        &[
            ("block_count", 32),
            ("embedding_length", 4096),
            ("attention.head_count", 32),
            ("attention.head_count_kv", 8),
        ]
    } else {
        &[]
    };
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend((2 + shape.len() as u64).to_le_bytes());
    string(&mut bytes, "general.architecture");
    bytes.extend(8u32.to_le_bytes());
    string(&mut bytes, architecture);
    string(&mut bytes, &format!("{architecture}.context_length"));
    bytes.extend(4u32.to_le_bytes());
    bytes.extend(training_context.to_le_bytes());
    for (key, value) in shape {
        string(&mut bytes, &format!("{architecture}.{key}"));
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

async fn server() -> Server {
    Server::configured(true, |config| {
        let template = config["models"][0].clone();
        let directory = std::path::PathBuf::from(template["model_file"].as_str().unwrap())
            .parent()
            .unwrap()
            .to_owned();
        let models = [
            (SOURCE, gguf("llama", 8192, false), 1),
            ("Demo-7B-GGUF-Q5_K_M", gguf("llama", 8192, false), 1),
            ("Demo-7B-GGUF-Q6_K", gguf("llama", 2048, false), 1),
            ("Demo-7B-GGUF-Q8_0", gguf("llama", 8192, false), 2),
            ("Demo-7B-GGUF-Q3_K_M", gguf("llama", 8192, true), 1),
            ("Demo-7B-GGUF-F16", gguf("qwen2", 8192, false), 1),
            ("Other-3B-GGUF-Q4_K_M", gguf("llama", 8192, false), 1),
        ];
        config["models"] = models
            .into_iter()
            .map(|(id, bytes, slots)| {
                let file = directory.join(format!("{id}.gguf"));
                std::fs::write(&file, bytes).unwrap();
                let mut model = template.clone();
                model["id"] = json!(id);
                model["model_file"] = json!(file);
                model["provider"] = json!("llama-cpp");
                model["max_inflight"] = json!(slots);
                model["args"] = json!([
                    "--port",
                    "{port}",
                    "--model",
                    "{model}",
                    "--events",
                    directory.join(format!("{id}.events")),
                    "--ctx-size",
                    "1024",
                    "--threads",
                    "2",
                    "--gpu-layers",
                    "0",
                    "--parallel",
                    slots.to_string(),
                ]);
                model
            })
            .collect();
    })
    .await
}

fn path(model: &str, suffix: &str) -> String {
    format!("models/{model}/{suffix}")
}

async fn revision(server: &Server, model: &str) -> Value {
    server.get(&path(model, "options")).await["revision"].clone()
}

async fn save_source(server: &Server, options: Value) -> Value {
    let (status, saved) = server
        .post(
            &path(SOURCE, "options"),
            json!({"expected_revision": revision(server, SOURCE).await, "options": options}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

async fn targets(server: &Server, models: &[&str]) -> Value {
    let mut result = Vec::new();
    for model in models {
        result.push(json!({"model": model, "expected_revision": revision(server, model).await}));
    }
    Value::Array(result)
}

fn result<'a>(reply: &'a Value, model: &str) -> &'a Value {
    reply["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["model"] == model)
        .unwrap_or_else(|| panic!("{model} missing from {reply}"))
}

#[tokio::test]
async fn variants_group_quants_and_copy_settings_in_one_generation_with_skip_reports() {
    let mut server = server().await;
    for prefix in PREFIXES {
        let (status, view) = server
            .json(
                Method::GET,
                &format!("{prefix}/models/{SOURCE}/variants"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["group"]["label"], "Demo-7B");
        let members: Vec<&str> = view["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member["model"].as_str().unwrap())
            .collect();
        assert_eq!(
            members,
            [
                "Demo-7B-GGUF-F16",
                "Demo-7B-GGUF-Q3_K_M",
                "Demo-7B-GGUF-Q5_K_M",
                "Demo-7B-GGUF-Q6_K",
                "Demo-7B-GGUF-Q8_0"
            ]
        );
    }
    let view = server.get(&path(SOURCE, "variants")).await;
    let f16 = &view["members"][0];
    assert_eq!(f16["eligible"], false);
    assert_eq!(f16["reason"], "different_architecture");
    assert_eq!(view["members"][2]["quant"], "Q5_K_M");
    assert_eq!(view["members"][2]["eligible"], true);
    assert_eq!(view["members"][2]["differs"], json!([]));
    let other = server.get(&path("Other-3B-GGUF-Q4_K_M", "variants")).await;
    assert_eq!(other["group"], Value::Null);
    assert_eq!(other["members"], json!([]));

    let settings = json!({"context_tokens":4096,"threads":3,"gpu_layers":0,"parallel_slots":1});
    let source = save_source(&server, settings.clone()).await;
    let view = server.get(&path(SOURCE, "variants")).await;
    assert_eq!(
        view["members"][2]["differs"],
        json!(["context_tokens", "threads", "gpu_layers", "parallel_slots"])
    );
    let policy_before = server.model("Demo-7B-GGUF-Q5_K_M").await["policy"].clone();
    let everyone = [
        "Demo-7B-GGUF-Q5_K_M",
        "Demo-7B-GGUF-Q6_K",
        "Demo-7B-GGUF-Q8_0",
        "Demo-7B-GGUF-Q3_K_M",
        "Demo-7B-GGUF-F16",
    ];
    let (status, reply) = server
        .post(
            &path(SOURCE, "variants"),
            json!({"expected_revision": source["revision"], "targets": targets(&server, &everyone).await}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["apply"], "next_load");
    let generation = reply["generation"].clone();
    assert!(generation.as_u64().unwrap() > source["revision"].as_u64().unwrap());

    let q5 = result(&reply, "Demo-7B-GGUF-Q5_K_M");
    assert_eq!(q5["outcome"], "updated");
    assert_eq!(q5["revision"], generation);
    assert_eq!(q5["skipped_fields"], json!([]));
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-Q5_K_M", "options")).await["saved"],
        settings
    );
    assert_eq!(
        server.model("Demo-7B-GGUF-Q5_K_M").await["policy"],
        policy_before
    );

    let q6 = result(&reply, "Demo-7B-GGUF-Q6_K");
    assert_eq!(q6["outcome"], "updated");
    assert_eq!(q6["revision"], generation);
    assert_eq!(
        q6["copied"],
        json!(["threads", "gpu_layers", "parallel_slots"])
    );
    assert_eq!(
        q6["skipped_fields"],
        json!([{"field":"context_tokens","reason":"context_exceeds_training","value":4096,"limit":2048}])
    );
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-Q6_K", "options")).await["saved"]["context_tokens"],
        Value::Null
    );

    let q8 = result(&reply, "Demo-7B-GGUF-Q8_0");
    assert_eq!(q8["outcome"], "updated");
    assert_eq!(
        q8["skipped_fields"][0]["reason"],
        "slot_resize_requires_registration"
    );
    assert_eq!(q8["skipped_fields"][0]["limit"], 2);
    let q8_options = server.get(&path("Demo-7B-GGUF-Q8_0", "options")).await;
    assert_eq!(q8_options["saved"]["threads"], 3);
    assert_eq!(q8_options["effective_launch"]["parallel_slots"], 2);

    let q3 = result(&reply, "Demo-7B-GGUF-Q3_K_M");
    assert_eq!(q3["skipped_fields"][0]["field"], "context_tokens");
    assert_eq!(q3["skipped_fields"][0]["reason"], "context_exceeds_budget");
    assert_eq!(q3["skipped_fields"][0]["limit"], 128);
    assert!(q3["skipped_fields"][0]["estimated_bytes"].as_u64().unwrap() > 128);

    let f16 = result(&reply, "Demo-7B-GGUF-F16");
    assert_eq!(f16["outcome"], "skipped");
    assert_eq!(f16["reason"], "different_architecture");
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-F16", "options")).await["saved"]["threads"],
        Value::Null
    );

    let (status, again) = server
        .post(
            &path(SOURCE, "variants"),
            json!({"expected_revision": source["revision"], "targets": targets(&server, &["Demo-7B-GGUF-Q5_K_M"]).await, "fields": ["threads"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["generation"], Value::Null);
    assert_eq!(
        result(&again, "Demo-7B-GGUF-Q5_K_M")["outcome"],
        "unchanged"
    );

    server.restart().await;
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-Q5_K_M", "options")).await["saved"],
        settings
    );
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-Q5_K_M", "options")).await["revision"],
        generation
    );
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-Q6_K", "options")).await["saved"]["threads"],
        3
    );
}

#[tokio::test]
async fn stale_revisions_and_foreign_targets_save_nothing() {
    let server = server().await;
    let source = save_source(&server, json!({"threads":5})).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    let stale_target = json!([{"model":"Demo-7B-GGUF-Q5_K_M","expected_revision":999}]);
    for (body, status, code) in [
        (
            json!({"expected_revision": source["revision"], "targets": [
                {"model":"Demo-7B-GGUF-Q8_0","expected_revision": revision(&server, "Demo-7B-GGUF-Q8_0").await},
                stale_target[0].clone()
            ]}),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
        (
            json!({"expected_revision": 0, "targets": targets(&server, &["Demo-7B-GGUF-Q5_K_M"]).await}),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
        (
            json!({"expected_revision": source["revision"], "targets": targets(&server, &["Other-3B-GGUF-Q4_K_M"]).await}),
            StatusCode::BAD_REQUEST,
            "not_a_variant",
        ),
        (
            json!({"expected_revision": source["revision"], "targets": targets(&server, &[SOURCE]).await}),
            StatusCode::BAD_REQUEST,
            "invalid_sync_targets",
        ),
        (
            json!({"expected_revision": source["revision"], "targets": []}),
            StatusCode::BAD_REQUEST,
            "invalid_sync_targets",
        ),
        (
            json!({"expected_revision": source["revision"], "targets": targets(&server, &["Demo-7B-GGUF-Q5_K_M"]).await, "fields": []}),
            StatusCode::BAD_REQUEST,
            "invalid_sync_fields",
        ),
    ] {
        let (actual, error) = server.post(&path(SOURCE, "variants"), body).await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["error"]["code"], code, "{error}");
        assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    }
    for body in [
        json!({"expected_revision": source["revision"], "targets": [], "fields": ["command"]}),
        json!({"expected_revision": source["revision"], "targets": [], "apply": "later"}),
        json!({"expected_revision": source["revision"], "targets": [], "args": []}),
    ] {
        let (status, _) = server.post(&path(SOURCE, "variants"), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(
        server.get(&path("Demo-7B-GGUF-Q8_0", "options")).await["saved"]["threads"],
        Value::Null
    );
    let (status, _) = server
        .json(
            Method::GET,
            &format!("/v1/models/{SOURCE}/variants"),
            None,
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn loaded_targets_need_reload_and_restart_with_the_copied_settings() {
    let server = server().await;
    let target = "Demo-7B-GGUF-Q5_K_M";
    assert_eq!(
        server.post("load", json!({"model": target})).await.0,
        StatusCode::OK
    );
    let old_pid = server.model(target).await["pid"].as_u64().unwrap();
    let settings = json!({"context_tokens":2048,"threads":3,"gpu_layers":0,"parallel_slots":1});
    let source = save_source(&server, settings.clone()).await;
    let body = |targets: Value, apply: &str| json!({"expected_revision": source["revision"], "targets": targets, "apply": apply});
    let (status, reply) = server
        .post(
            &path(SOURCE, "variants"),
            body(targets(&server, &[target]).await, "next_load"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(result(&reply, target)["outcome"], "skipped");
    assert_eq!(result(&reply, target)["reason"], "worker_in_use");
    assert_eq!(reply["generation"], Value::Null);
    assert_eq!(server.model(target).await["pid"], old_pid);

    let (status, reply) = server
        .post(
            &path(SOURCE, "variants"),
            body(
                targets(&server, &[target, "Demo-7B-GGUF-Q8_0"]).await,
                "reload",
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["apply"], "reload");
    let loaded = result(&reply, target);
    assert_eq!(loaded["outcome"], "updated");
    assert_eq!(loaded["restarted"], true);
    assert_eq!(result(&reply, "Demo-7B-GGUF-Q8_0")["restarted"], false);
    assert_eq!(server.model("Demo-7B-GGUF-Q8_0").await["loaded"], false);
    let model = server.model(target).await;
    assert_eq!(model["loaded"], true);
    assert_eq!(model["revision"], reply["generation"]);
    let new_pid = model["pid"].as_u64().unwrap();
    assert_ne!(new_pid, old_pid);
    let observed = server.get(&format!("juiced/context?model={target}")).await;
    assert_eq!(observed["applied"]["launch"], settings);
}
