mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server, artifacts::Source, filesystem::tree};

async fn server() -> Server {
    Server::configured(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http")
    })
    .await
}
// The paging view is per process; restart comparisons cover durable state only.
async fn inventory(server: &Server) -> Value {
    let mut value = server.get("juiced/artifacts").await;
    value.as_object_mut().unwrap().remove("view");
    value
}
async fn acquire(server: &Server, source: &Source) -> String {
    let generation = inventory(server).await["generation"].clone();
    let (status, job) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":source.download()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let path = format!("juiced/artifacts/jobs/{}", job["id"].as_str().unwrap());
    server
        .wait_for(async || {
            let job = server.get(&path).await;
            job["state"] == "complete" && job["writer_active"] == false
        })
        .await;
    server.get(&path).await["artifact"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn specification(
    model: &str,
    model_bytes: usize,
    companion: &str,
    companion_bytes: usize,
    revision: &str,
) -> Value {
    json!({"schema_version":1,"origin":{"importer_id":"rust-process-fixture","importer_version":"1","source_revision":revision},"primary":"model","members":[{"name":"projector","role":"projector","artifact":companion,"bytes":companion_bytes},{"name":"model","role":"model","artifact":model,"bytes":model_bytes}]})
}

async fn primary_set(server: &Server, artifact: &str, bytes: usize, revision: &str) -> String {
    let mut spec = specification(artifact, bytes, artifact, bytes, revision);
    spec["members"]
        .as_array_mut()
        .unwrap()
        .retain(|member| member["role"] == "model");
    let generation = inventory(server).await["generation"].clone();
    let (status, record) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":spec}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{record}");
    record["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn model_set_promotion_and_rollback_preserve_policy_identity_references_and_cached_continuation()
 {
    let mut server = Server::configured(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["cache_enabled"] = json!(true);
    })
    .await;
    let source = Source::with_bytes(b"original model bytes".to_vec()).await;
    let replacement = Source::with_bytes(b"replacement model bytes".to_vec()).await;
    let artifact = acquire(&server, &source).await;
    let other = acquire(&server, &replacement).await;
    let first = primary_set(&server, &artifact, source.bytes.len(), "first").await;
    let second = primary_set(&server, &other, replacement.bytes.len(), "second").await;
    let third = primary_set(&server, &artifact, source.bytes.len(), "third").await;
    let registration = json!({"id":"revision-model","artifact":artifact,"artifact_set":first,"model_file":server.config["models"][0]["model_file"],
        "provider":"llama-cpp","command":server.config["models"][0]["command"],
        "args":["--port","{port}","--model","{model}","--slot-save-path","{cache}","--ctx-size","256","--parallel","1","--n-gpu-layers","0"],
        "resident_bytes":16,"max_inflight":1,"operations":["completions"],"policy":"disabled","revision":0});
    let (status, registered) = server.post("models/register", registration).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let path = "models/revision-model/artifacts";
    for prefix in PREFIXES {
        let (status, view) = server
            .json(
                Method::GET,
                &format!("{prefix}/{path}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            view["current"],
            json!({"artifact":artifact,"artifact_set":first})
        );
        assert_eq!(view["previous"], Value::Null);
        assert_eq!(view["supported"], true);
        assert_eq!(view["read_only"], true);
    }
    let (status, no_rollback) = server
        .post(
            path,
            json!({"expected_revision":registered["revision"],"change":{"action":"rollback"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{no_rollback}");
    assert_eq!(
        no_rollback["error"]["code"],
        "artifact_rollback_unavailable"
    );
    let (status, enabled) = server.post("juiced/policy",json!({"model":"revision-model","policy":"on-demand","expected_revision":registered["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    async fn complete(server: &Server) {
        let response = server
            .client
            .post(format!("{}/v1/completions", server.base))
            .bearer_auth(server.admin_key().unwrap())
            .header("x-juiced-session", "revision-session")
            .json(&json!({"model":"revision-model","prompt":"pipeline","max_tokens":2}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap()["model"],
            "revision-model"
        );
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
    }
    complete(&server).await;
    let original = server.get("juiced/context?model=revision-model").await;
    let cache = server.get("juiced/cache").await;
    let (status,saved) = server.post("juiced/cache/save",json!({"model":"revision-model","slot":0,"session":"revision-session","expected_generation":cache["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let configured = std::fs::read(&server.config_path).unwrap();
    let before = inventory(&server).await;
    let (status,busy) = server.post(path,json!({"expected_revision":enabled["revision"],"change":{"action":"promote","artifact_set":second}})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{busy}");
    assert_eq!(busy["error"]["code"], "worker_in_use");
    assert_eq!(std::fs::read(&server.config_path).unwrap(), configured);
    assert_eq!(inventory(&server).await, before);
    assert_eq!(
        server
            .post("unload", json!({"model":"revision-model"}))
            .await
            .0,
        StatusCode::OK
    );
    let revision = server.get(path).await["revision"].clone();
    let mut companion_spec = specification(
        &other,
        replacement.bytes.len(),
        &artifact,
        source.bytes.len(),
        "non-executable-companions",
    );
    companion_spec["origin"]["importer_version"] = json!("1");
    let generation = inventory(&server).await["generation"].clone();
    let (status, companion_set) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":companion_spec}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{companion_set}");
    for (target, status, code) in [
        (
            first.clone(),
            StatusCode::CONFLICT,
            "artifact_revision_unchanged",
        ),
        (
            format!("set-{}", "f".repeat(64)),
            StatusCode::NOT_FOUND,
            "artifact_not_found",
        ),
        (
            companion_set["id"].as_str().unwrap().to_owned(),
            StatusCode::NOT_IMPLEMENTED,
            "artifact_set_execution_unsupported",
        ),
    ] {
        let before = inventory(&server).await;
        let disk = std::fs::read(&server.config_path).unwrap();
        let (actual,error)=server.post(path,json!({"expected_revision":revision,"change":{"action":"promote","artifact_set":target}})).await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["error"]["code"], code);
        assert_eq!(inventory(&server).await, before);
        assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    }
    let file = server
        .directory
        .path()
        .join("artifacts")
        .join(format!("artifact-{other}.bin"));
    let bytes = std::fs::read(&file).unwrap();
    let mut damaged = bytes.clone();
    damaged[0] ^= 1;
    std::fs::write(&file, &damaged).unwrap();
    let before = inventory(&server).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    let (status,error)=server.post(path,json!({"expected_revision":revision,"change":{"action":"promote","artifact_set":second}})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "artifact_hash_mismatch");
    assert_eq!(inventory(&server).await, before);
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(std::fs::read(&file).unwrap(), damaged);
    std::fs::write(&file, bytes).unwrap();
    let mut old = first.clone();
    let mut previous = None;
    for (prefix, target) in PREFIXES.into_iter().zip([&second, &third, &first, &second]) {
        let current = server.get(path).await;
        let (status,promoted) = server.json(Method::POST,&format!("{prefix}/{path}"),Some(json!({"expected_revision":current["revision"],"change":{"action":"promote","artifact_set":target}})),server.admin_key()).await;
        assert_eq!(status, StatusCode::OK, "{promoted}");
        assert_eq!(promoted["model"], "revision-model");
        assert_eq!(promoted["policy"], "on-demand");
        assert_eq!(promoted["current"]["artifact_set"], *target);
        assert_eq!(promoted["previous"]["artifact_set"], old);
        assert!(promoted["revision"].as_u64().unwrap() > current["revision"].as_u64().unwrap());
        assert_eq!(promoted["obsolete_reference_retained"], false);
        for set in [&first, &second, &third] {
            let view = server.get(&format!("juiced/artifacts/sets/{set}")).await;
            assert_eq!(
                view["references"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("model:revision-model")),
                set == target || *set == old
            );
        }
        if let Some(obsolete) = previous
            .as_ref()
            .filter(|set: &&String| *set != target && **set != old)
        {
            let view = server
                .get(&format!("juiced/artifacts/sets/{obsolete}"))
                .await;
            assert_eq!(view["references"], json!([]));
        }
        let configured = std::fs::read(&server.config_path).unwrap();
        let before = inventory(&server).await;
        let (status, stale) = server
            .post(
                path,
                json!({"expected_revision":current["revision"],"change":{"action":"rollback"}}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{stale}");
        assert_eq!(stale["error"]["code"], "revision_conflict");
        assert_eq!(std::fs::read(&server.config_path).unwrap(), configured);
        assert_eq!(inventory(&server).await, before);
        previous = Some(old);
        old = target.clone();
    }
    server.restart().await;
    let current = server.get(path).await;
    assert_eq!(current["current"]["artifact_set"], second);
    assert_eq!(current["previous"]["artifact_set"], first);
    complete(&server).await;
    let changed = server.get("juiced/context?model=revision-model").await;
    assert_ne!(
        original["applied"]["launch_plan"]["semantic_fingerprint"],
        changed["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    let cache_tree = tree(&server.directory.path().join("kv-cache"));
    let restore = json!({"model":"revision-model","slot":0,"session":"revision-session","snapshot":saved["snapshot"]["id"]});
    let (status, mismatch) = server.post("juiced/cache/restore", restore.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "snapshot_identity_mismatch");
    assert_eq!(tree(&server.directory.path().join("kv-cache")), cache_tree);
    assert_eq!(
        server
            .post("unload", json!({"model":"revision-model"}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, rolled_back) = server
        .post(
            path,
            json!({"expected_revision":current["revision"],"change":{"action":"rollback"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rolled_back}");
    assert_eq!(rolled_back["current"]["artifact_set"], first);
    assert_eq!(rolled_back["previous"]["artifact_set"], second);
    assert_eq!(rolled_back["policy"], "on-demand");
    let (status, restored) = server.post("juiced/cache/restore", restore).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    complete(&server).await;
    let resumed = server.get("juiced/context?model=revision-model").await;
    assert_eq!(
        original["applied"]["launch_plan"]["semantic_fingerprint"],
        resumed["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"revision-model"}))
            .await
            .0,
        StatusCode::OK
    );
    let (status,disabled) = server.post("juiced/policy",json!({"model":"revision-model","policy":"disabled","expected_revision":rolled_back["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":"revision-model","expected_revision":disabled["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["artifact_reference_retained"], false);
    let retained = inventory(&server).await;
    assert_eq!(
        retained["charged_bytes"].as_u64().unwrap(),
        (source.bytes.len() + replacement.bytes.len()) as u64
    );
    for set in retained["artifact_sets"].as_array().unwrap() {
        assert_eq!(set["references"], json!([]));
        assert_eq!(set["active_holds"], 0);
    }
    server.restart().await;
    assert_eq!(
        inventory(&server).await["artifact_sets"],
        retained["artifact_sets"]
    );
}

#[tokio::test]
async fn complete_set_publication_read_restart_and_removal_use_all_prefixes_and_preserve_payloads()
{
    let mut server = server().await;
    let model = Source::with_bytes(b"owned model fixture bytes".to_vec()).await;
    let companion = Source::with_bytes(b"owned projector fixture bytes".to_vec()).await;
    let model_id = acquire(&server, &model).await;
    let companion_id = acquire(&server, &companion).await;
    let root = server.directory.path().join("artifacts");
    for (index, prefix) in PREFIXES.into_iter().enumerate() {
        let before = inventory(&server).await;
        let spec = specification(
            &model_id,
            model.bytes.len(),
            &companion_id,
            companion.bytes.len(),
            &format!("revision-{index}"),
        );
        let (status, published) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/sets"),
                Some(json!({"expected_generation":before["generation"],"specification":spec})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{published}");
        let set = published["id"].as_str().unwrap();
        assert!(set.starts_with("set-") && set.len() == 68);
        assert_eq!(published["compatibility"], "unqualified");
        assert_eq!(published["content_verification"], "required-on-acquire");
        assert_eq!(published["active_holds"], 0);
        assert_eq!(published["specification"]["members"][0]["name"], "model");
        let complete = inventory(&server).await;
        assert_eq!(
            complete["generation"].as_u64().unwrap(),
            before["generation"].as_u64().unwrap() + 1
        );
        assert_eq!(complete["artifact_sets"].as_array().unwrap().len(), 1);
        for artifact in complete["artifacts"].as_array().unwrap() {
            assert_eq!(artifact["active_holds"], 0);
            assert!(
                artifact["references"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("artifact-set:{set}")))
            );
        }
        for read_prefix in PREFIXES {
            let (status, observed) = server
                .json(
                    Method::GET,
                    &format!("{read_prefix}/juiced/artifacts/sets/{set}"),
                    None,
                    server.admin_key(),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(observed, published);
        }
        let recorded = tree(&root);
        let (status, repeated) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/sets"),
                Some(json!({"expected_generation":complete["generation"],"specification":spec})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{repeated}");
        assert_eq!(repeated, published);
        assert_eq!(tree(&root), recorded);
        let (status, stale) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/sets/remove"),
                Some(json!({"expected_generation":before["generation"],"set":set})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{stale}");
        assert_eq!(stale["error"]["code"], "artifact_revision_conflict");
        for artifact in [&model_id, &companion_id] {
            let (status, blocked) = server
                .post(
                    "juiced/artifacts/remove",
                    json!({"expected_generation":complete["generation"],"artifact":artifact}),
                )
                .await;
            assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
            assert_eq!(blocked["error"]["code"], "artifact_busy");
        }
        assert_eq!(tree(&root), recorded);
        server.restart().await;
        assert_eq!(
            server.get(&format!("juiced/artifacts/sets/{set}")).await,
            published
        );
        assert_eq!(tree(&root), recorded);
        let (status, removed) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/sets/remove"),
                Some(json!({"expected_generation":complete["generation"],"set":set})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{removed}");
        assert_eq!(removed["artifacts_preserved"], true);
        let remaining = inventory(&server).await;
        assert!(remaining["artifact_sets"].as_array().unwrap().is_empty());
        assert_eq!(remaining["charged_bytes"], complete["charged_bytes"]);
        for (artifact, bytes) in [
            (&model_id, &*model.bytes),
            (&companion_id, &*companion.bytes),
        ] {
            assert_eq!(
                std::fs::read(root.join(format!("artifact-{artifact}.bin"))).unwrap(),
                *bytes
            );
        }
        let (status, missing) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/artifacts/sets/{set}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"]["code"], "artifact_not_found");
    }
    for artifact in [&model_id, &companion_id] {
        let generation = inventory(&server).await["generation"].clone();
        assert_eq!(
            server
                .post(
                    "juiced/artifacts/remove",
                    json!({"artifact":artifact,"expected_generation":generation})
                )
                .await
                .0,
            StatusCode::OK
        );
    }
    assert_eq!(inventory(&server).await["charged_bytes"], 0);
    assert!(
        server.get("juiced/catalog").await["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model["loaded"] == false)
    );
}

#[tokio::test]
async fn incomplete_corrupt_and_malformed_set_requests_never_publish_or_change_the_owned_inputs() {
    let server = server().await;
    let model = Source::with_bytes(b"unchanged model fixture".to_vec()).await;
    let companion = Source::with_bytes(b"unchanged companion fixture".to_vec()).await;
    let model_id = acquire(&server, &model).await;
    let companion_id = companion.download()["expected_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let spec = specification(
        &model_id,
        model.bytes.len(),
        &companion_id,
        companion.bytes.len(),
        "negative-revision",
    );
    let root = server.directory.path().join("artifacts");
    for prefix in PREFIXES {
        let before = inventory(&server).await;
        let bytes = tree(&root);
        let (status, missing) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/sets"),
                Some(json!({"expected_generation":before["generation"],"specification":spec})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
        assert_eq!(missing["error"]["code"], "artifact_not_found");
        assert_eq!(inventory(&server).await, before);
        assert_eq!(tree(&root), bytes);
    }
    acquire(&server, &companion).await;
    let path = root.join(format!("artifact-{companion_id}.bin"));
    let mut corrupted = companion.bytes.as_ref().clone();
    corrupted[0] ^= 1;
    std::fs::write(&path, &corrupted).unwrap();
    for prefix in PREFIXES {
        let before = inventory(&server).await;
        let bytes = tree(&root);
        let (status, refused) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/sets"),
                Some(json!({"expected_generation":before["generation"],"specification":spec})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{refused}");
        assert_eq!(refused["error"]["code"], "artifact_hash_mismatch");
        assert_eq!(inventory(&server).await, before);
        assert_eq!(tree(&root), bytes);
    }
    std::fs::write(&path, &*companion.bytes).unwrap();
    let before = inventory(&server).await;
    let bytes = tree(&root);
    for defect in ["role", "duplicate", "primary", "unknown", "size"] {
        let mut invalid = spec.clone();
        match defect {
            "role" => invalid["members"][0]["role"] = json!("unqualified-secret-role"),
            "duplicate" => {
                let member = invalid["members"][0].clone();
                invalid["members"].as_array_mut().unwrap().push(member);
            }
            "primary" => invalid["primary"] = json!("missing"),
            "unknown" => invalid["private-future-field"] = json!("SECRET_PRIVATE_SET_INPUT"),
            _ => invalid["members"][0]["bytes"] = json!(0),
        }
        let (status, error) = server
            .post(
                "juiced/artifacts/sets",
                json!({"expected_generation":before["generation"],"specification":invalid}),
            )
            .await;
        let expected = if matches!(defect, "role" | "unknown") {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::BAD_REQUEST
        };
        assert_eq!(status, expected, "{defect}: {error}");
        assert!(!error.to_string().contains("SECRET_PRIVATE_SET_INPUT"));
    }
    assert_eq!(inventory(&server).await, before);
    assert_eq!(tree(&root), bytes);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn primary_set_registration_inference_cache_restart_and_revision_change_preserve_identity_authority()
 {
    let mut server = Server::configured(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["cache_enabled"] = json!(true);
    })
    .await;
    let source = Source::with_bytes(support::metadata_gguf(8192)).await;
    let artifact = acquire(&server, &source).await;
    let mut spec = specification(
        &artifact,
        source.bytes.len(),
        &artifact,
        source.bytes.len(),
        "primary-revision-1",
    );
    spec["members"]
        .as_array_mut()
        .unwrap()
        .retain(|member| member["role"] == "model");
    let generation = inventory(&server).await["generation"].clone();
    let (status, first) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":spec}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let set = first["id"].as_str().unwrap().to_owned();
    let mut registration = server.config["models"][0].clone();
    registration["id"] = json!("set-bound-model");
    registration["artifact"] = json!(artifact);
    registration["artifact_set"] = json!(set);
    registration["provider"] = json!("llama-cpp");
    registration["max_inflight"] = json!(1);
    registration["operations"] = json!(["completions"]);
    registration["policy"] = json!("disabled");
    registration["revision"] = json!(0);
    registration["args"].as_array_mut().unwrap().extend([
        json!("--ctx-size"),
        json!("256"),
        json!("--parallel"),
        json!("1"),
        json!("--n-gpu-layers"),
        json!("0"),
        json!("--slot-save-path"),
        json!("{cache}"),
    ]);
    let mut companion = spec.clone();
    companion["members"].as_array_mut().unwrap().push(json!({"name":"projector","role":"projector","artifact":artifact,"bytes":source.bytes.len()}));
    let generation = inventory(&server).await["generation"].clone();
    let (status, extra) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":companion}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{extra}");
    for defect in ["primary", "companions", "missing-primary"] {
        let mut invalid = registration.clone();
        invalid["id"] = json!(format!("refused-{defect}"));
        let (expected, code) = match defect {
            "primary" => {
                invalid["artifact"] = json!("f".repeat(64));
                (StatusCode::CONFLICT, "artifact_set_binding_mismatch")
            }
            "companions" => {
                invalid["artifact_set"] = extra["id"].clone();
                (
                    StatusCode::NOT_IMPLEMENTED,
                    "artifact_set_execution_unsupported",
                )
            }
            _ => {
                invalid["artifact"] = Value::Null;
                (StatusCode::BAD_REQUEST, "invalid_registration")
            }
        };
        let config = std::fs::read(&server.config_path).unwrap();
        let before = inventory(&server).await;
        let (status, error) = server.post("models/register", invalid).await;
        assert_eq!(status, expected, "{defect}: {error}");
        assert_eq!(error["error"]["code"], code);
        assert_eq!(std::fs::read(&server.config_path).unwrap(), config);
        assert_eq!(inventory(&server).await, before);
    }
    let (status, registered) = server.post("models/register", registration).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert_eq!(registered["artifact_set"], set);
    let set_view = server.get(&format!("juiced/artifacts/sets/{set}")).await;
    assert_eq!(set_view["references"], json!(["model:set-bound-model"]));
    assert_eq!(set_view["active_holds"], 1);
    let(status,enabled)=server.post("juiced/policy",json!({"model":"set-bound-model","policy":"on-demand","expected_revision":registered["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let session = "set-bound-cache-session";
    async fn complete(server: &Server, session: &str) {
        let response = server
            .client
            .post(format!("{}/v1/completions", server.base))
            .bearer_auth(server.admin_key().unwrap())
            .header("x-juiced-session", session)
            .json(&json!({"model":"set-bound-model","prompt":"bounded pipeline","max_tokens":4}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let output: Value = response.json().await.unwrap();
        assert_eq!(output["model"], "set-bound-model");
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
    }
    complete(&server, session).await;
    let before = server.get("juiced/context?model=set-bound-model").await;
    assert_eq!(before["applied"]["launch_plan"]["artifact_set"], set);
    let cache = server.get("juiced/cache").await;
    let(status,saved)=server.post("juiced/cache/save",json!({"model":"set-bound-model","slot":0,"session":session,"expected_generation":cache["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["snapshot"]["state"], "saved");
    assert_eq!(
        server
            .post("unload", json!({"model":"set-bound-model"}))
            .await
            .0,
        StatusCode::OK
    );
    server.restart().await;
    let(status,restored)=server.post("juiced/cache/restore",json!({"model":"set-bound-model","slot":0,"session":session,"snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    complete(&server, session).await;
    let resumed = server.get("juiced/context?model=set-bound-model").await;
    assert_eq!(
        resumed["applied"]["launch_plan"]["semantic_fingerprint"],
        before["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"set-bound-model"}))
            .await
            .0,
        StatusCode::OK
    );
    spec["origin"]["source_revision"] = json!("primary-revision-2");
    let generation = inventory(&server).await["generation"].clone();
    let (status, second) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":spec}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_ne!(second["id"], set);
    server.stop();
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    config["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["id"] == "set-bound-model")
        .unwrap()["artifact_set"] = second["id"].clone();
    std::fs::write(&server.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    server.start().await;
    assert_eq!(
        server
            .post("load", json!({"model":"set-bound-model"}))
            .await
            .0,
        StatusCode::OK
    );
    let changed = server.get("juiced/context?model=set-bound-model").await;
    assert_ne!(
        changed["applied"]["launch_plan"]["semantic_fingerprint"],
        before["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    let root = server.directory.path().join("kv-cache");
    let retained = tree(&root);
    let(status,mismatch)=server.post("juiced/cache/restore",json!({"model":"set-bound-model","slot":0,"session":session,"snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "snapshot_identity_mismatch");
    assert_eq!(tree(&root), retained);
    complete(&server, session).await;
    assert_eq!(
        server
            .post("unload", json!({"model":"set-bound-model"}))
            .await
            .0,
        StatusCode::OK
    );
    let current = server.model("set-bound-model").await;
    let(status,disabled)=server.post("juiced/policy",json!({"model":"set-bound-model","policy":"disabled","expected_revision":current["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let current = server.model("set-bound-model").await;
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":"set-bound-model","expected_revision":current["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["artifact_reference_retained"], false);
    let second_view = server
        .get(&format!(
            "juiced/artifacts/sets/{}",
            second["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(second_view["references"], json!([]));
    assert_eq!(second_view["active_holds"], 0);
    let generation = inventory(&server).await["generation"].clone();
    assert_eq!(
        server
            .post(
                "juiced/artifacts/sets/remove",
                json!({"set":second["id"],"expected_generation":generation})
            )
            .await
            .0,
        StatusCode::OK
    );
}
