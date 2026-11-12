mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{Server, canonical_model, rejected_manager};

fn canonical_config(mut config: Value) -> Value {
    config["schema_version"] = json!(2);
    for model in config["models"].as_array_mut().unwrap() {
        *model = canonical_model(model.take());
    }
    config
}
fn saved(server: &Server) -> Value {
    serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap()
}
fn assert_canonical(config: &Value) {
    assert_eq!(config["schema_version"], 2);
    for model in config["models"].as_array().unwrap() {
        for legacy in ["provider", "runtime_bundle", "command", "args", "options"] {
            assert!(model.get(legacy).is_none(), "legacy field {legacy}");
        }
        let profile = &model["execution_profile"];
        assert_eq!(profile["schema_version"], 1);
        assert_eq!(profile["placement"], "unresolved");
        assert!(profile["command"].is_string());
        assert!(profile["args"].is_array());
        assert!(profile["options"].is_object());
        assert!(profile.get("implementation_id").is_some());
    }
}
fn llama(model: &mut Value) {
    model["provider"] = json!("llama-cpp");
    model["max_inflight"] = json!(1);
    model["options"] = json!({"context_tokens":768,"threads":2,"gpu_layers":0,"parallel_slots":1});
}
async fn load_and_infer(server: &Server, model: &str) -> Value {
    let (status, loaded) = server.post("load", json!({"model":model})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let (status, response) = server
        .post(
            "completions",
            json!({"model":model,"prompt":"Profile data pipeline"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["model"], model);
    loaded
}

#[tokio::test]
async fn legacy_reads_preserve_config_bytes_then_policy_and_options_saves_publish_canonical_profiles()
 {
    let mut server = Server::prepare(true, |config| {
        config["generation"] = json!(41);
        for (model, revision) in config["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip([17, 5, 2])
        {
            model["revision"] = json!(revision);
        }
        llama(&mut config["models"][0]);
    });
    let legacy = server.config.clone();
    let original = std::fs::read(&server.config_path).unwrap();
    server.start().await;
    for model in legacy["models"].as_array().unwrap() {
        let observed = server.model(model["id"].as_str().unwrap()).await;
        for field in ["id", "policy", "revision"] {
            assert_eq!(observed[field], model[field]);
        }
        assert_eq!(observed["loaded"], false);
    }
    assert_eq!(
        server.get("models/fixture.one/options").await["saved"],
        legacy["models"][0]["options"]
    );
    assert!(server.get("juiced/context?model=fixture.one").await["applied"].is_null());
    assert_eq!(std::fs::read(&server.config_path).unwrap(), original);
    server.restart().await;
    assert_eq!(std::fs::read(&server.config_path).unwrap(), original);
    let (status, changed) = server
        .post(
            "juiced/policy",
            json!({"model":"fixture.two","policy":"disabled","expected_revision":5}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let disk = saved(&server);
    assert_canonical(&disk);
    for (actual, old) in disk["models"]
        .as_array()
        .unwrap()
        .iter()
        .zip(legacy["models"].as_array().unwrap())
    {
        assert_eq!(actual["id"], old["id"]);
        assert_eq!(actual["model_file"], old["model_file"]);
        assert_eq!(actual["execution_profile"]["args"], old["args"]);
        assert_eq!(actual["execution_profile"]["command"], old["command"]);
        if old["id"] != "fixture.two" {
            assert_eq!(actual["policy"], old["policy"]);
            assert_eq!(actual["revision"], old["revision"]);
        }
    }
    assert_eq!(
        disk["models"][0]["execution_profile"]["implementation_id"],
        "llama-cpp"
    );
    assert_eq!(
        disk["models"][1]["execution_profile"]["implementation_id"],
        Value::Null
    );
    server.restart().await;
    assert_eq!(
        server.model("fixture.two").await["revision"],
        changed["revision"]
    );
    load_and_infer(&server, "fixture.one").await;
    assert_eq!(
        server.get("juiced/context?model=fixture.one").await["applied"]["launch"]["context_tokens"],
        768
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    let current = server.model("fixture.one").await;
    let requested = json!({"context_tokens":1024,"threads":3,"gpu_layers":0,"parallel_slots":1});
    let (status, options) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":current["revision"],"options":requested}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{options}");
    let disk = saved(&server);
    assert_canonical(&disk);
    assert_eq!(disk["models"][0]["execution_profile"]["options"], requested);
    server.restart().await;
    assert_eq!(
        server.get("models/fixture.one/options").await["saved"],
        requested
    );
    load_and_infer(&server, "fixture.one").await;
    assert_eq!(
        server.get("juiced/context?model=fixture.one").await["observed"]["props"]["per_request_tokens"],
        1024
    );
}

#[tokio::test]
async fn nested_and_legacy_registration_launch_after_restart_and_invalid_profiles_have_no_side_effects()
 {
    let mut server = Server::new(true).await;
    for (id, nested) in [("nested-profile", true), ("legacy-profile", false)] {
        let mut registration = server.config["models"][0].clone();
        registration["id"] = json!(id);
        registration["policy"] = json!("disabled");
        registration["revision"] = json!(0);
        registration["args"][5] = json!(server.directory.path().join(format!("{id}.events")));
        llama(&mut registration);
        let registration = if nested {
            canonical_model(registration)
        } else {
            registration
        };
        let (status, registered) = server.post("models/register", registration).await;
        assert_eq!(status, StatusCode::OK, "{registered}");
        assert_eq!(registered["policy"], "disabled");
        assert_eq!(registered["implementation_id"], "llama-cpp");
        let (status, enabled) = server
            .post(
                "juiced/policy",
                json!({"model":id,"policy":"on-demand","expected_revision":registered["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{enabled}");
    }
    let stable = std::fs::read(&server.config_path).unwrap();
    let before = server.get("juiced/catalog").await;
    let mut valid = server.config["models"][0].clone();
    valid["id"] = json!("rejected-profile");
    valid["policy"] = json!("disabled");
    valid["revision"] = json!(0);
    llama(&mut valid);
    let valid = canonical_model(valid);
    for (target, field, value) in [
        ("profile", "unexpected", json!(true)),
        ("profile", "args", json!([])),
        ("profile", "schema_version", json!(2)),
        ("profile", "implementation_id", json!("whisper-cpp")),
        ("profile", "placement", json!("gpu:0")),
        ("model", "provider", json!("llama-cpp")),
        ("model", "command", json!("untrusted-command")),
        ("model", "unexpected", json!(true)),
    ] {
        let mut rejected = valid.clone();
        if target == "profile" {
            rejected["execution_profile"][field] = value;
        } else {
            rejected[field] = value;
        }
        let (status, error) = server.post("models/register", rejected).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert!(error["error"]["code"].is_string());
        assert_eq!(std::fs::read(&server.config_path).unwrap(), stable);
        assert_eq!(server.get("juiced/catalog").await, before);
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
        assert!(!server.event("fixture.one", "worker-startup", "started"));
    }
    assert_canonical(&saved(&server));
    server.restart().await;
    for id in ["nested-profile", "legacy-profile"] {
        let loaded = load_and_infer(&server, id).await;
        assert_eq!(loaded["implementation_id"], "llama-cpp");
        assert_eq!(
            server.post("unload", json!({"model":id})).await.0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn invalid_schema_two_configs_exit_before_listener_or_hot_worker_and_preserve_input_bytes() {
    for invalid in [
        "legacy-schema-two",
        "mixed",
        "unknown-model",
        "unknown-config",
        "unknown-profile",
        "wrong-implementation",
        "wrong-profile-version",
        "wrong-config-version",
        "nested-schema-one",
    ] {
        let server = Server::prepare(false, |_| {});
        let mut config = canonical_config(server.config.clone());
        config["models"][0]["policy"] = json!("hot");
        match invalid {
            "legacy-schema-two" => {
                config = server.config.clone();
                config["schema_version"] = json!(2);
                config["models"][0]["policy"] = json!("hot");
            }
            "mixed" => config["models"][0]["command"] = json!("ignored-legacy-command"),
            "unknown-model" => config["models"][0]["unexpected"] = json!(true),
            "unknown-config" => config["unexpected"] = json!(true),
            "unknown-profile" => {
                config["models"][0]["execution_profile"]["unexpected"] = json!(true)
            }
            "wrong-implementation" => {
                config["models"][0]["execution_profile"]["implementation_id"] = json!("llama-cpp")
            }
            "wrong-profile-version" => {
                config["models"][0]["execution_profile"]["schema_version"] = json!(2)
            }
            "wrong-config-version" => config["schema_version"] = json!(3),
            "nested-schema-one" => config["schema_version"] = json!(1),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec_pretty(&config).unwrap();
        std::fs::write(&server.config_path, &bytes).unwrap();
        let output = rejected_manager(&server.config_path).await;
        assert!(!output.stderr.is_empty(), "{invalid}");
        assert_eq!(
            std::fs::read(&server.config_path).unwrap(),
            bytes,
            "{invalid}"
        );
        for model in ["fixture.one", "fixture.two", "fixture.disabled"] {
            assert!(
                !server.event(model, "worker-startup", "started"),
                "{invalid}"
            );
        }
    }
}

fn startup_port(server: &Server, pid: &Value) -> u16 {
    let event = std::fs::read_to_string(server.directory.path().join("fixture.one.events"))
        .unwrap()
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| {
            event["id"] == "worker-startup" && event["event"] == "started" && event["pid"] == *pid
        })
        .expect("startup event for the owned worker generation");
    event["port"].as_u64().unwrap().try_into().unwrap()
}
async fn execution_fingerprint(server: &Server) -> Value {
    let context = server.get("juiced/context?model=fixture.one").await;
    let fingerprint = context["applied"]["execution_fingerprint"].clone();
    assert_eq!(fingerprint.as_str().unwrap().len(), 64);
    fingerprint
}

#[tokio::test]
async fn launch_fingerprint_excludes_ports_pids_revisions_and_migration_but_binds_semantic_options()
{
    let mut server = Server::configured(true, |config| llama(&mut config["models"][0])).await;
    let first = load_and_infer(&server, "fixture.one").await;
    let fingerprint = execution_fingerprint(&server).await;
    let first_port = startup_port(&server, &first["pid"]);
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    let held_port = tokio::net::TcpListener::bind(("127.0.0.1", first_port))
        .await
        .expect("retain the unloaded worker port during replacement");
    let (status,disabled)=server.post("juiced/policy",json!({"model":"fixture.one","policy":"disabled","expected_revision":first["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let (status,enabled)=server.post("juiced/policy",json!({"model":"fixture.one","policy":"on-demand","expected_revision":disabled["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_canonical(&saved(&server));
    let second = load_and_infer(&server, "fixture.one").await;
    assert_ne!(second["pid"], first["pid"]);
    assert_ne!(startup_port(&server, &second["pid"]), first_port);
    assert!(
        second["process_lifecycle"]["generation"].as_u64().unwrap()
            > first["process_lifecycle"]["generation"].as_u64().unwrap()
    );
    assert_ne!(second["revision"], first["revision"]);
    assert_eq!(execution_fingerprint(&server).await, fingerprint);
    drop(held_port);
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    let changed_options =
        json!({"context_tokens":1536,"threads":2,"gpu_layers":0,"parallel_slots":1});
    let (status, updated) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":second["revision"],"options":changed_options}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let third = load_and_infer(&server, "fixture.one").await;
    assert!(
        third["process_lifecycle"]["generation"].as_u64().unwrap()
            > second["process_lifecycle"]["generation"].as_u64().unwrap()
    );
    let changed = execution_fingerprint(&server).await;
    assert_ne!(changed, fingerprint);
    assert_eq!(
        server.get("juiced/context?model=fixture.one").await["applied"]["launch"]["context_tokens"],
        1536
    );
    server.restart().await;
    let restarted = load_and_infer(&server, "fixture.one").await;
    assert_ne!(restarted["pid"], third["pid"]);
    assert_eq!(execution_fingerprint(&server).await, changed);
}

fn owned_weights(server: &Server) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(server.config["models"][0]["model_file"].as_str().unwrap());
    assert!(path.starts_with(server.directory.path()));
    path
}

#[tokio::test]
async fn changed_retained_weights_during_startup_refuse_ready_and_reconcile_owned_process() {
    let server = Server::configured(true, |config| {
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--startup-delay-ms"), json!("750")]);
    })
    .await;
    let path = owned_weights(&server);
    let original = std::fs::read(&path).unwrap();
    let client = server.client.clone();
    let url = format!("{}/v1/load", server.base);
    let key = server.admin_key().unwrap();
    let pending = tokio::spawn(async move {
        let response = client
            .post(url)
            .bearer_auth(key)
            .json(&json!({"model":"fixture.one"}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json::<Value>().await.unwrap())
    });
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let loading = server.model("fixture.one").await;
    assert_eq!(loading["state"], "loading");
    let pid = loading["process_lifecycle"]["pid"].as_u64().unwrap() as u32;
    let mut changed = original.clone();
    changed[0] ^= 1;
    std::fs::write(&path, &changed).unwrap();
    let (status, error) = tokio::time::timeout(std::time::Duration::from_secs(4), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "model_artifact_changed");
    server
        .wait_for(async || {
            let model = server.model("fixture.one").await;
            model["process_lifecycle"].is_null()
                && model["loaded"] == false
                && server.get("juiced/state").await["resources"]["reserved_bytes"] == 0
        })
        .await;
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
    let refused = server.model("fixture.one").await;
    assert!(refused["pid"].is_null());
    assert!(refused["runtime_identity"].is_null());
    assert!(!server.event("fixture.one", "request", "started"));
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    std::fs::write(&path, &original).unwrap();
    let recovered = load_and_infer(&server, "fixture.one").await;
    assert_eq!(recovered["state"], "ready");
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_loaded_weights_refuse_cache_before_engine_dispatch_and_reloading_revalidates_original_bytes()
 {
    let server = Server::configured(true, |config| {
        config["cache_enabled"] = json!(true);
        llama(&mut config["models"][0]);
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--slot-save-path"), json!("{cache}")]);
    })
    .await;
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", "profile-model-guard")
        .json(&json!({"model":"fixture.one","prompt":"Retained model cache pipeline"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let cache = server.get("juiced/cache").await;
    let body = json!({"model":"fixture.one","slot":0,"session":"profile-model-guard","expected_generation":cache["generation"],"maximum_bytes":1024});
    let (status, saved) = server.post("juiced/cache/save", body).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    let snapshot_path = server.directory.path().join("kv-cache").join(format!(
        "kv-{}.bin",
        saved["snapshot"]["id"].as_str().unwrap()
    ));
    let snapshot_bytes = std::fs::read(&snapshot_path).unwrap();
    let before = server.get("juiced/cache").await;
    let calls = server.event_count("fixture.one", "slot-save", "started");
    let ready = server.model("fixture.one").await;
    let path = owned_weights(&server);
    let original = std::fs::read(&path).unwrap();
    let mut changed = original.clone();
    changed[0] ^= 1;
    std::fs::write(&path, &changed).unwrap();
    let body = json!({"model":"fixture.one","slot":0,"session":"profile-model-guard","expected_generation":before["generation"],"maximum_bytes":1024});
    let (status, error) = server.post("juiced/cache/save", body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "model_artifact_changed");
    assert_eq!(
        server.event_count("fixture.one", "slot-save", "started"),
        calls
    );
    assert_eq!(server.get("juiced/cache").await, before);
    assert_eq!(std::fs::read(&snapshot_path).unwrap(), snapshot_bytes);
    let stale = server.model("fixture.one").await;
    assert_eq!(stale["pid"], ready["pid"]);
    assert_eq!(stale["runtime_identity"]["state"], "stale");
    assert_eq!(
        stale["runtime_identity"]["reason"],
        "model_artifact_changed"
    );
    std::fs::write(&path, &original).unwrap();
    let (status, error) = server.post("juiced/cache/save", body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "model_artifact_changed");
    assert_eq!(
        server.event_count("fixture.one", "slot-save", "started"),
        calls
    );
    assert_eq!(server.get("juiced/cache").await, before);
    let (status, unloaded) = server.post("unload", json!({"model":"fixture.one"})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unload after weight restoration: {unloaded}"
    );
    let reload_started = std::time::Instant::now();
    let (status, loaded) = server.post("load", json!({"model":"fixture.one"})).await;
    let reload_elapsed = reload_started.elapsed();
    assert_eq!(
        status,
        StatusCode::OK,
        "reload after weight restoration: {loaded}; elapsed={reload_elapsed:?}; model={}; admission={}",
        server.model("fixture.one").await,
        server.get("juiced/admission").await,
    );
    let (status,restored)=server.post("juiced/cache/restore",json!({"model":"fixture.one","slot":0,"session":"profile-model-guard","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
    assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
    assert_eq!(std::fs::read(&snapshot_path).unwrap(), snapshot_bytes);
}

#[tokio::test]
async fn selected_llama_public_aliases_bind_identity_even_when_every_launch_input_is_shared() {
    let server = Server::configured(true, |config| {
        let mut first = config["models"][0].clone();
        llama(&mut first);
        let mut second = first.clone();
        second["id"] = json!("fixture.public-two");
        config["models"] = json!([first, second]);
    })
    .await;
    let first = load_and_infer(&server, "fixture.one").await;
    let first_context = server.get("juiced/context?model=fixture.one").await;
    let (status, _) = server.post("unload", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK);
    let second = load_and_infer(&server, "fixture.public-two").await;
    let second_context = server.get("juiced/context?model=fixture.public-two").await;
    assert_eq!(first_context["configured"], second_context["configured"]);
    assert_ne!(
        first_context["applied"]["launch_plan"]["semantic_fingerprint"],
        second_context["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(format!("/proc/{}/cmdline", second["pid"].as_u64().unwrap()))
            .unwrap()
            .take(65537)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= 65536);
        let arguments = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| **argument == b"--alias")
                .count(),
            1
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == [b"--alias".as_slice(), b"fixture.public-two".as_slice()])
        );
    }
    assert_ne!(first["pid"], second["pid"]);
}

#[tokio::test]
async fn selected_llama_refuses_multi_alias_ids_before_spawn_and_preserves_registration() {
    let server = Server::configured(true, |config| {
        let mut model = config["models"][0].clone();
        llama(&mut model);
        model["id"] = json!("fixture.one,foreign");
        config["models"] = json!([model]);
    })
    .await;
    let bytes = std::fs::read(&server.config_path).unwrap();
    let (status, error) = server
        .post("load", json!({"model":"fixture.one,foreign"}))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "native_public_alias_unsupported");
    assert!(server.model("fixture.one,foreign").await["pid"].is_null());
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert!(!server.event("fixture.one", "worker-startup", "started"));
    assert_eq!(std::fs::read(&server.config_path).unwrap(), bytes);
}
