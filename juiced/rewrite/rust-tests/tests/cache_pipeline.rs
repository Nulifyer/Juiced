mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use support::PREFIXES;
use support::Server;

async fn cache_server(authenticated: bool, enabled: bool) -> Server {
    Server::configured(authenticated, |config| configure_cache(config, enabled)).await
}

fn configure_cache(config: &mut Value, enabled: bool) {
    config["cache_enabled"] = json!(enabled);
    for model in config["models"].as_array_mut().unwrap() {
        model["max_inflight"] = json!(1);
        model["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--slot-save-path"), json!("{cache}")]);
    }
}

async fn view(server: &Server) -> Value {
    let (status, value) = server
        .json(Method::GET, "/v1/juiced/cache", None, server.admin_key())
        .await;
    assert_eq!(status, StatusCode::OK);
    value
}

async fn complete(server: &Server, session: &str, path: &str) -> Value {
    let mut request = server.client.post(format!("{}/v1/{path}", server.base))
        .header("x-juiced-session", session)
        .json(&json!({"model":"fixture.one","prompt":"Cache pipeline fixture", "max_tokens":4, "stream":false}));
    if let Some(key) = server.admin_key() {
        request = request.header("x-api-key", key);
    }
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    body
}

#[cfg(target_os = "linux")]
async fn save(server: &Server, session: &str) -> Value {
    let cache = view(server).await;
    let (status, result) = server.post("juiced/cache/save", json!({"model":"fixture.one","slot":0,"session":session,"expected_generation":cache["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::OK, "cache save: {result}");
    assert_eq!(result["saved"], true);
    assert!(
        result["engine"]["n_saved"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        result["engine"]["n_written"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert_eq!(result["engine"]["id_slot"], 0);
    assert!(result["engine"].get("filename").is_none());
    result["snapshot"].clone()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn explicit_cache_save_restore_inspect_forget_works_across_all_prefixes() {
    let server = cache_server(true, true).await;
    for (index, prefix) in PREFIXES.iter().enumerate() {
        let session = format!("cache-prefix-{index}");
        let completion = complete(&server, &session, "completions").await;
        let generation = view(&server).await["generation"].clone();
        let (status, saved) = server.json(Method::POST, &format!("{prefix}/juiced/cache/save"), Some(json!({"model":"fixture.one","slot":0,"session":session,"expected_generation":generation,"maximum_bytes":1024})), server.admin_key()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
        assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
        let snapshot = &saved["snapshot"];
        assert_eq!(snapshot["state"], "saved");
        let expected = format!(
            "{:x}",
            Sha256::digest(
                completion["received_body_sha256"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
            )
        );
        assert_eq!(snapshot["sha256"], expected);
        let (status, inspection) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/cache/inspect"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(inspection["read_only"], true);
        assert_eq!(inspection["recovery_authorized"], false);
        let inspected = inspection["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["snapshot"]["id"] == snapshot["id"])
            .unwrap();
        assert_eq!(inspected["checksum_valid"], true);
        let (status, restored) = server.json(Method::POST, &format!("{prefix}/juiced/cache/restore"), Some(json!({"model":"fixture.one","slot":0,"session":session,"snapshot":snapshot["id"]})), server.admin_key()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(restored["restored"], true);
        assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
        assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
        assert_eq!(server.json(Method::POST, &format!("{prefix}/juiced/cache/forget"), Some(json!({"snapshot":snapshot["id"],"session":session,"expected_revision":snapshot["revision"]})), server.admin_key()).await.1["forgotten"], true);
        assert!(view(&server).await["data"].as_array().unwrap().is_empty());
        let config = view(&server).await["configuration_generation"].clone();
        assert_eq!(
            server
                .json(
                    Method::POST,
                    &format!("{prefix}/juiced/cache"),
                    Some(json!({"enabled":true,"expected_generation":config})),
                    server.admin_key()
                )
                .await
                .0,
            StatusCode::OK
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn engine_cache_bytes_survive_unload_and_manager_restart_then_restore_and_continue() {
    let mut server = cache_server(true, true).await;
    complete(&server, "persistent-session", "completions").await;
    let snapshot = save(&server, "persistent-session").await;
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    server.restart().await;
    assert_eq!(view(&server).await["enabled"], true);
    assert_eq!(view(&server).await["data"][0]["id"], snapshot["id"]);
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    let (status, restored) = server.post("juiced/cache/restore", json!({"model":"fixture.one","slot":0,"session":"persistent-session","snapshot":snapshot["id"]})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
    assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
    let continuation = complete(&server, "persistent-session", "completions").await;
    assert_eq!(continuation["fixture_restores"], 1);
    let (status, error) = server
        .json(Method::GET, "/v1/juiced/cache", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["error"]["code"], "operator_required");
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.read_key().unwrap())
        .header("x-juiced-session", "persistent-session")
        .json(&json!({"model":"fixture.one","prompt":"Separate inference principal"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["fixture_restores"],
        1
    );
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let owned = view(&server).await;
    assert_eq!(owned["data"].as_array().unwrap().len(), 1);
    assert_eq!(owned["data"][0]["id"], snapshot["id"]);
    let(status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"persistent-session","expected_generation":owned["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "cache_context_mismatch");
    assert_eq!(view(&server).await, owned);
    assert_eq!(server.post("juiced/cache/forget", json!({"snapshot":snapshot["id"],"session":"persistent-session","expected_revision":snapshot["revision"]})).await.0, StatusCode::OK);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn wrong_session_protocol_or_rotated_epoch_cannot_restore_saved_context() {
    let mut server = cache_server(true, true).await;
    complete(&server, "private-session", "completions").await;
    let snapshot = save(&server, "private-session").await;
    let generation = view(&server).await["generation"].clone();
    for (path, request) in [
        (
            "juiced/cache/save",
            json!({"model":"fixture.one","slot":0,"session":"wrong-session","expected_generation":generation,"maximum_bytes":1024}),
        ),
        (
            "juiced/cache/restore",
            json!({"model":"fixture.one","slot":0,"session":"wrong-session","snapshot":snapshot["id"]}),
        ),
        (
            "juiced/cache/forget",
            json!({"snapshot":snapshot["id"],"session":"wrong-session","expected_revision":snapshot["revision"]}),
        ),
    ] {
        assert_eq!(
            server.post(path, request).await.0,
            StatusCode::CONFLICT,
            "{path} accepted another session"
        );
    }
    complete(&server, "private-session", "chat/completions").await;
    assert_eq!(server.post("juiced/cache/save", json!({"model":"fixture.one","slot":0,"session":"private-session","expected_generation":generation,"maximum_bytes":1024})).await.1["error"]["code"], "cache_context_mismatch");
    assert_eq!(view(&server).await["generation"], generation);
    let old_key = server.admin_key().unwrap();
    server.rotate_credentials().await;
    assert_eq!(
        server
            .json(Method::GET, "/v1/juiced/cache", None, Some(old_key))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(view(&server).await["data"], json!([]));
    let (status, error) = server.post("juiced/cache/restore", json!({"model":"fixture.one","slot":0,"session":"private-session","snapshot":snapshot["id"]})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "snapshot_not_found");
    let inspection = server
        .json(
            Method::GET,
            "/v1/juiced/cache/inspect",
            None,
            server.admin_key(),
        )
        .await
        .1;
    assert!(
        inspection["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["snapshot"]["id"] == snapshot["id"])
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cache_policy_generation_and_snapshot_revision_fail_closed_without_deleting_bytes() {
    let server = cache_server(true, false).await;
    complete(&server, "policy-session", "completions").await;
    assert_eq!(server.post("juiced/cache/save", json!({"model":"fixture.one","slot":0,"session":"policy-session","expected_generation":0,"maximum_bytes":1024})).await.1["error"]["code"], "cache_disabled");
    let initial = view(&server).await;
    assert_eq!(
        server
            .post(
                "juiced/cache",
                json!({"enabled":true,"expected_generation":initial["configuration_generation"]})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post(
                "juiced/cache",
                json!({"enabled":false,"expected_generation":initial["configuration_generation"]})
            )
            .await
            .0,
        StatusCode::CONFLICT
    );
    let snapshot = save(&server, "policy-session").await;
    let before = view(&server).await;
    assert_eq!(server.post("juiced/cache/save", json!({"model":"fixture.one","slot":0,"session":"policy-session","expected_generation":0,"maximum_bytes":1024})).await.1["error"]["code"], "cache_revision_conflict");
    assert_eq!(server.post("juiced/cache/forget", json!({"snapshot":snapshot["id"],"session":"policy-session","expected_revision":snapshot["revision"].as_u64().unwrap()+1})).await.1["error"]["code"], "cache_revision_conflict");
    assert_eq!(view(&server).await, before);
    assert_eq!(
        server
            .post(
                "juiced/cache",
                json!({"enabled":false,"expected_generation":before["configuration_generation"]})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(server.post("juiced/cache/restore", json!({"model":"fixture.one","slot":0,"session":"policy-session","snapshot":snapshot["id"]})).await.1["error"]["code"], "cache_disabled");
    assert_eq!(server.post("juiced/cache/forget", json!({"snapshot":snapshot["id"],"session":"policy-session","expected_revision":snapshot["revision"]})).await.0, StatusCode::OK);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn corrupt_cache_bytes_are_detected_without_inspection_mutation_or_removal() {
    let server = cache_server(true, true).await;
    complete(&server, "corruption-session", "completions").await;
    let snapshot = save(&server, "corruption-session").await;
    let root = server.directory.path().join("cache");
    let path = root.join(format!("kv-{}.bin", snapshot["id"].as_str().unwrap()));
    let before = std::fs::read(root.join("snapshots.json")).unwrap();
    let size = std::fs::metadata(&path).unwrap().len() as usize;
    std::fs::write(&path, vec![b'z'; size]).unwrap();
    let (status, error) = server.post("juiced/cache/restore", json!({"model":"fixture.one","slot":0,"session":"corruption-session","snapshot":snapshot["id"]})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"]["code"], "snapshot_corrupt");
    let inspection = server
        .json(
            Method::GET,
            "/v1/juiced/cache/inspect",
            None,
            server.admin_key(),
        )
        .await
        .1;
    assert_eq!(inspection["snapshots"][0]["checksum_valid"], false);
    assert_eq!(inspection["read_only"], true);
    assert_eq!(inspection["recovery_authorized"], false);
    assert_eq!(std::fs::read(root.join("snapshots.json")).unwrap(), before);
    assert!(path.is_file());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn pending_or_oversized_snapshot_writes_remain_visible_and_charged() {
    let server = cache_server(true, true).await;
    complete(&server, "bounded-session", "completions").await;
    let generation = view(&server).await["generation"].clone();
    let (status, error) = server.post("juiced/cache/save", json!({"model":"fixture.one","slot":0,"session":"bounded-session","expected_generation":generation,"maximum_bytes":1})).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(error["error"]["code"], "cache_limit");
    let inspection = server
        .json(
            Method::GET,
            "/v1/juiced/cache/inspect",
            None,
            server.admin_key(),
        )
        .await
        .1;
    assert_eq!(inspection["snapshots"][0]["snapshot"]["state"], "uncertain");
    assert!(inspection["charged_bytes"].as_u64().unwrap() >= 64);
    assert_eq!(inspection["recovery_authorized"], false);
    let pending = &inspection["snapshots"][0]["snapshot"];
    assert_eq!(server.post("juiced/cache/forget", json!({"snapshot":pending["id"],"session":"bounded-session","expected_revision":pending["revision"]})).await.1["error"]["code"], "snapshot_uncertain");
}

#[cfg(target_os = "linux")]
async fn inspection(server: &Server) -> Value {
    let (status, value) = server
        .json(
            Method::GET,
            "/v1/juiced/cache/inspect",
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    value
}

#[cfg(target_os = "linux")]
async fn oversized_write(server: &Server, session: &str) -> Value {
    complete(server, session, "completions").await;
    let generation = view(server).await["generation"].clone();
    let (status, error) = server
        .post("juiced/cache/save", json!({"model":"fixture.one","slot":0,"session":session,"expected_generation":generation,"maximum_bytes":1}))
        .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(error["error"]["code"], "cache_limit");
    view(server).await["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["state"] == "uncertain")
        .expect("oversized write retains its reservation")
        .clone()
}

#[cfg(target_os = "linux")]
fn writer_facts_are_private(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(
                    ![
                        "writer",
                        "actor",
                        "manager_owner",
                        "store_id",
                        "binding_digest",
                        "domain",
                        "token"
                    ]
                    .contains(&key.as_str()),
                    "private writer field exposed: {key}"
                );
                writer_facts_are_private(value);
            }
        }
        Value::Array(values) => values.iter().for_each(writer_facts_are_private),
        _ => {}
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn exact_writer_exit_authorizes_recovery_without_removing_saved_sibling_all_prefixes() {
    let server = cache_server(true, true).await;
    complete(&server, "saved-sibling", "completions").await;
    let sibling = save(&server, "saved-sibling").await;
    let root = server.directory.path().join("cache");
    let sibling_path = root.join(format!("kv-{}.bin", sibling["id"].as_str().unwrap()));
    let sibling_bytes = std::fs::read(&sibling_path).unwrap();
    let sibling_charge = inspection(&server).await["charged_bytes"].as_u64().unwrap();

    for (index, prefix) in PREFIXES.iter().enumerate() {
        let session = format!("recover-owned-{index}");
        let pending = oversized_write(&server, &session).await;
        assert_eq!(pending["writer_bound"], true);
        assert_eq!(pending["writer_current_owner"], true);
        assert_eq!(pending["cleanup_eligible"], false);
        let pending_path = root.join(format!("kv-{}.bin", pending["id"].as_str().unwrap()));
        let pending_bytes = std::fs::read(&pending_path).unwrap();
        let manifest_before = std::fs::read(root.join("snapshots.json")).unwrap();
        let manifest: Value = serde_json::from_slice(&manifest_before).unwrap();
        let record = &manifest["entries"][pending["id"].as_str().unwrap()];
        assert_eq!(record["reserved_bytes"], 1);
        assert_eq!(record["writer"]["model"], "fixture.one");
        let write_key = record["writer"]["write_key"].as_str().unwrap();
        assert_eq!(write_key.len(), 32);
        assert!(
            write_key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert!(record["writer"]["generation"].as_u64().unwrap() > 0);
        assert_eq!(
            record["writer"]["binding_digest"].as_str().unwrap().len(),
            64
        );
        let before = view(&server).await;
        let inspected = inspection(&server).await;
        assert!(
            inspected["charged_bytes"].as_u64().unwrap()
                >= sibling_charge + pending_bytes.len() as u64
        );
        assert_eq!(inspected["read_only"], true);
        assert_eq!(inspected["recovery_authorized"], false);
        writer_facts_are_private(&before);
        writer_facts_are_private(&inspected);
        let pid = server.model("fixture.one").await["pid"].clone();
        let request = json!({"snapshot":pending["id"],"session":session,"expected_revision":pending["revision"]});
        let path = format!("{prefix}/juiced/cache/recover");
        for (body, key, expected, code) in [
            (
                request.clone(),
                server.admin_key(),
                StatusCode::CONFLICT,
                "snapshot_uncertain",
            ),
            (
                json!({"snapshot":pending["id"],"session":"another-session","expected_revision":pending["revision"]}),
                server.admin_key(),
                StatusCode::CONFLICT,
                "snapshot_identity_mismatch",
            ),
            (
                json!({"snapshot":pending["id"],"session":session,"expected_revision":pending["revision"].as_u64().unwrap()+1}),
                server.admin_key(),
                StatusCode::CONFLICT,
                "cache_revision_conflict",
            ),
            (
                request.clone(),
                server.read_key(),
                StatusCode::UNAUTHORIZED,
                "operator_required",
            ),
            (
                request.clone(),
                Some("invalid-recovery-credential"),
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
        ] {
            let (status, error) = server.json(Method::POST, &path, Some(body), key).await;
            assert_eq!(status, expected, "{path}: {error}");
            assert_eq!(error["error"]["code"], code, "{path}");
            writer_facts_are_private(&error);
        }
        assert_eq!(view(&server).await, before);
        assert_eq!(server.model("fixture.one").await["pid"], pid);
        assert_eq!(
            std::fs::read(root.join("snapshots.json")).unwrap(),
            manifest_before
        );
        assert_eq!(std::fs::read(&pending_path).unwrap(), pending_bytes);
        assert_eq!(
            server
                .post("unload", json!({"model":"fixture.one"}))
                .await
                .0,
            StatusCode::OK
        );
        let (status, recovered) = server
            .json(Method::POST, &path, Some(request), server.admin_key())
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {recovered}");
        assert_eq!(recovered["recovered"], true);
        assert_eq!(recovered["snapshot"], pending["id"]);
        writer_facts_are_private(&recovered);
        assert!(!pending_path.exists());
        assert_eq!(view(&server).await["data"], json!([sibling.clone()]));
        assert_eq!(inspection(&server).await["charged_bytes"], sibling_charge);
        assert_eq!(std::fs::read(&sibling_path).unwrap(), sibling_bytes);
    }
    assert_eq!(server.post("juiced/cache/restore", json!({"model":"fixture.one","slot":0,"session":"saved-sibling","snapshot":sibling["id"]})).await.0, StatusCode::OK);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn prior_manager_recovery_uses_captured_actor_and_preserves_unavailable_proof() {
    let mut server = cache_server(true, true).await;
    let pending = oversized_write(&server, "prior-manager").await;
    let root = server.directory.path().join("cache");
    let path = root.join(format!("kv-{}.bin", pending["id"].as_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();
    let charge = inspection(&server).await["charged_bytes"].clone();
    let actor_present = pending["writer_actor_present"].as_bool().unwrap();
    server.restart().await;
    let retained = view(&server).await;
    assert_eq!(retained["data"][0]["writer_current_owner"], false);
    assert_eq!(retained["data"][0]["id"], pending["id"]);
    assert_eq!(inspection(&server).await["charged_bytes"], charge);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let request = json!({"snapshot":pending["id"],"session":"prior-manager","expected_revision":pending["revision"]});
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (status, result) = loop {
        let result = server.post("juiced/cache/recover", request.clone()).await;
        if actor_present
            && result.0 == StatusCode::CONFLICT
            && result.1["error"]["code"] == "snapshot_uncertain"
        {
            assert!(
                std::time::Instant::now() < deadline,
                "prior writer exit was not confirmed"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(inspection(&server).await["charged_bytes"], charge);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            continue;
        }
        break result;
    };
    writer_facts_are_private(&result);
    if actor_present {
        assert_eq!(status, StatusCode::OK, "captured actor recovery: {result}");
        assert_eq!(result["recovered"], true);
        assert!(!path.exists());
        assert_eq!(inspection(&server).await["charged_bytes"], 0);
    } else {
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(result["error"]["code"], "snapshot_uncertain");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(inspection(&server).await["charged_bytes"], charge);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn credential_transition_cannot_recover_previous_namespace_writer() {
    let mut server = cache_server(true, true).await;
    let pending = oversized_write(&server, "private-writer").await;
    let root = server.directory.path().join("cache");
    let path = root.join(format!("kv-{}.bin", pending["id"].as_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();
    let charge = inspection(&server).await["charged_bytes"].clone();
    let old_key = server.admin_key();
    server.rotate_credentials().await;
    let manifest = std::fs::read(root.join("snapshots.json")).unwrap();
    let request = json!({"snapshot":pending["id"],"session":"private-writer","expected_revision":pending["revision"]});
    assert_eq!(
        server
            .json(
                Method::POST,
                "/v1/juiced/cache/recover",
                Some(request.clone()),
                old_key
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, result) = server.post("juiced/cache/recover", request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(result["error"]["code"], "snapshot_not_found");
    writer_facts_are_private(&result);
    assert_eq!(view(&server).await["data"], json!([]));
    assert_eq!(inspection(&server).await["charged_bytes"], charge);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::read(root.join("snapshots.json")).unwrap(),
        manifest
    );
}

#[cfg(target_os = "linux")]
async fn reader_server() -> Server {
    Server::configured(true, |config| {
        configure_cache(config, true);
        config["request_timeout_ms"] = json!(12000);
    })
    .await
}

#[cfg(target_os = "linux")]
async fn reader_fixture_base(server: &Server) -> String {
    let pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
    let events =
        std::fs::read_to_string(server.directory.path().join("fixture.one.events")).unwrap();
    let event = events
        .lines()
        .rev()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["id"] == "worker-startup" && event["pid"] == pid)
        .unwrap();
    let port = u16::try_from(event["port"].as_u64().unwrap()).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    assert_eq!(reader_fixture_state(server, &base).await["pid"], pid);
    base
}

#[cfg(target_os = "linux")]
async fn reader_fixture_state(server: &Server, base: &str) -> Value {
    let response = server
        .client
        .get(format!("{base}/__fixture/state"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["schema_version"], 1);
    value
}

#[cfg(target_os = "linux")]
async fn reader_fixture_control(server: &Server, base: &str, body: Value) -> Value {
    let response = server
        .client
        .post(format!("{base}/__fixture/state/control"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[cfg(target_os = "linux")]
async fn reader_restore_socket(
    server: &Server,
    snapshot: &Value,
    session: &str,
) -> tokio::net::TcpStream {
    use tokio::io::AsyncWriteExt;
    let mut socket = tokio::net::TcpStream::connect(server.base.trim_start_matches("http://"))
        .await
        .unwrap();
    let body = json!({"model":"fixture.one","slot":0,"session":session,"snapshot":snapshot["id"]})
        .to_string();
    socket.write_all(format!("POST /v1/juiced/cache/restore HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}",server.admin_key().unwrap(),body.len()).as_bytes()).await.unwrap();
    socket
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn restore_reader_is_durable_before_effect_and_retained_until_valid_original_eof() {
    let server = reader_server().await;
    let session = "reader-held-eof";
    complete(&server, session, "completions").await;
    let snapshot = save(&server, session).await;
    let root = server.directory.path().join("cache");
    let path = root.join(format!("kv-{}.bin", snapshot["id"].as_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();
    let charge = inspection(&server).await["charged_bytes"].clone();
    let base = reader_fixture_base(&server).await;
    let armed = reader_fixture_control(&server, &base, json!({"action":"arm","operation":"restore","hold_effect":true,"hold_eof":true,"reply":"valid","status":200})).await;
    let operation = armed["operation_id"].as_u64().unwrap();
    let request = server
        .client
        .post(format!("{}/v1/juiced/cache/restore", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .json(&json!({"model":"fixture.one","slot":0,"session":session,"snapshot":snapshot["id"]}));
    let restore = tokio::spawn(async move {
        let response = request.send().await.unwrap();
        (response.status(), response.json::<Value>().await.unwrap())
    });
    server
        .wait_for(async || {
            let state = reader_fixture_state(&server, &base).await;
            state["jobs"]["current"]["id"] == operation
                && state["jobs"]["current"]["physical_started"] == true
        })
        .await;
    let job = reader_fixture_state(&server, &base).await;
    assert_eq!(job["jobs"]["current"]["effect_started"], false);
    let held = view(&server).await["data"][0].clone();
    assert_eq!(held["state"], "saved");
    assert_eq!(held["active_readers"], 1);
    let reader = held["retained_readers"][0].clone();
    assert_eq!(reader["writer_current_owner"], true);
    assert_eq!(reader["release_eligible"], false);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("snapshots.json")).unwrap()).unwrap();
    let intent = &manifest["reader_intents"][reader["id"].as_str().unwrap()];
    assert_eq!(intent["purpose"], "restore");
    assert_eq!(intent["snapshot"], snapshot["id"]);
    assert_eq!(intent["bytes"], snapshot["bytes"]);
    assert_eq!(intent["sha256"], snapshot["sha256"]);
    let (status, error) = server.post("juiced/cache/forget", json!({"snapshot":snapshot["id"],"session":session,"expected_revision":snapshot["revision"]})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "snapshot_busy");
    reader_fixture_control(
        &server,
        &base,
        json!({"action":"release-effect","operation_id":operation}),
    )
    .await;
    server
        .wait_for(async || {
            let state = reader_fixture_state(&server, &base).await;
            state["jobs"]["current"]["physical_settled"] == true
                && state["jobs"]["current"]["reply_started"] == true
        })
        .await;
    assert!(!restore.is_finished());
    let job = reader_fixture_state(&server, &base).await;
    assert_eq!(job["jobs"]["current"]["reply_eof"], false);
    let before_eof = view(&server).await;
    assert_eq!(before_eof["data"][0]["retained_readers"], json!([reader]));
    assert_eq!(before_eof["data"][0]["active_readers"], 1);
    writer_facts_are_private(&before_eof);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    reader_fixture_control(
        &server,
        &base,
        json!({"action":"release-eof","operation_id":operation}),
    )
    .await;
    let (status, restored) = tokio::time::timeout(std::time::Duration::from_secs(3), restore)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["restored"], true);
    assert_eq!(restored["retained_reader_cleanup_pending"], false);
    assert_eq!(restored["snapshot"]["retained_readers"], json!([]));
    let settled = view(&server).await;
    assert_eq!(settled["data"][0]["retained_readers"], json!([]));
    assert_eq!(settled["data"][0]["active_readers"], 0);
    assert_eq!(settled["data"][0]["revision"], snapshot["revision"]);
    assert_eq!(settled["data"][0]["sha256"], snapshot["sha256"]);
    assert_eq!(inspection(&server).await["charged_bytes"], charge);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn restarted_reader_requires_authenticated_reader_cas_recovery_before_restore_autoload() {
    let mut server = reader_server().await;
    let session = "reader-restart";
    complete(&server, session, "completions").await;
    let snapshot = save(&server, session).await;
    let root = server.directory.path().join("cache");
    let path = root.join(format!("kv-{}.bin", snapshot["id"].as_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();
    let charge = inspection(&server).await["charged_bytes"].clone();
    let base = reader_fixture_base(&server).await;
    let armed = reader_fixture_control(&server,&base,json!({"action":"arm","operation":"restore","hold_effect":true,"hold_eof":true,"reply":"valid","status":200})).await;
    let operation = armed["operation_id"].as_u64().unwrap();
    let socket = reader_restore_socket(&server, &snapshot, session).await;
    server
        .wait_for(async || {
            let state = reader_fixture_state(&server, &base).await;
            state["jobs"]["current"]["id"] == operation
                && state["jobs"]["current"]["physical_started"] == true
        })
        .await;
    let held = view(&server).await["data"][0].clone();
    let reader = held["retained_readers"][0].clone();
    assert_eq!(held["active_readers"], 1);
    assert_eq!(reader["release_eligible"], false);
    assert_ne!(reader["revision"], snapshot["revision"]);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("snapshots.json")).unwrap()).unwrap();
    let saved_record = manifest["entries"][snapshot["id"].as_str().unwrap()].clone();
    let intent = manifest["reader_intents"][reader["id"].as_str().unwrap()].clone();
    assert_eq!(intent["snapshot"], snapshot["id"]);
    assert_eq!(intent["binding"]["model"], "fixture.one");
    assert!(intent["binding"]["generation"].as_u64().unwrap() > 0);
    let actor_present = reader["writer_actor_present"].as_bool().unwrap();
    let owned = server.model("fixture.one").await;
    assert_eq!(
        intent["binding"]["generation"],
        owned["process_lifecycle"]["generation"]
    );
    if actor_present {
        assert_eq!(intent["binding"]["actor"]["pid"], owned["pid"]);
    }
    server.kill_manager_only();
    drop(socket);
    server.retire_reaped_manager_without_group_signal();
    server.start().await;
    let before = view(&server).await;
    assert_eq!(before["data"][0]["state"], "saved");
    assert_eq!(before["data"][0]["active_readers"], 0);
    assert_eq!(before["data"][0]["retained_readers"][0]["id"], reader["id"]);
    assert_eq!(
        before["data"][0]["retained_readers"][0]["writer_current_owner"],
        false
    );
    assert_eq!(
        before["data"][0]["retained_readers"][0]["release_eligible"],
        false
    );
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    let starts = server.event_count("fixture.one", "worker-startup", "started");
    let restore =
        json!({"model":"fixture.one","slot":0,"session":session,"snapshot":snapshot["id"]});
    for (route, body) in [
        (
            "juiced/cache/forget",
            json!({"snapshot":snapshot["id"],"session":session,"expected_revision":snapshot["revision"]}),
        ),
        ("juiced/cache/restore", restore.clone()),
    ] {
        let (status, error) = server.post(route, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
    }
    assert_eq!(
        server.event_count("fixture.one", "worker-startup", "started"),
        starts
    );
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    let request = json!({"snapshot":snapshot["id"],"reader":reader["id"],"session":session,"expected_revision":reader["revision"]});
    let manifest_before = std::fs::read(root.join("snapshots.json")).unwrap();
    for (body, key, expected, code) in [
        (
            request.clone(),
            server.read_key(),
            StatusCode::UNAUTHORIZED,
            "operator_required",
        ),
        (
            request.clone(),
            Some("invalid-reader-recovery-key"),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            json!({"snapshot":snapshot["id"],"reader":reader["id"],"session":"wrong-reader-session","expected_revision":reader["revision"]}),
            server.admin_key(),
            StatusCode::CONFLICT,
            "snapshot_identity_mismatch",
        ),
        (
            json!({"snapshot":snapshot["id"],"reader":reader["id"],"session":session,"expected_revision":snapshot["revision"]}),
            server.admin_key(),
            StatusCode::CONFLICT,
            "cache_revision_conflict",
        ),
    ] {
        let (status, error) = server
            .json(Method::POST, "/v1/juiced/cache/recover", Some(body), key)
            .await;
        assert_eq!(status, expected, "{error}");
        assert_eq!(error["error"]["code"], code);
    }
    assert_eq!(
        std::fs::read(root.join("snapshots.json")).unwrap(),
        manifest_before
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(inspection(&server).await["charged_bytes"], charge);
    writer_facts_are_private(&before);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (status, result) = loop {
        let result = server.post("juiced/cache/recover", request.clone()).await;
        if actor_present
            && result.0 == StatusCode::CONFLICT
            && result.1["error"]["code"] == "snapshot_uncertain"
        {
            assert!(
                std::time::Instant::now() < deadline,
                "prior reader exit was not confirmed"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(inspection(&server).await["charged_bytes"], charge);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            continue;
        }
        break result;
    };
    writer_facts_are_private(&result);
    if !actor_present {
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(result["error"]["code"], "snapshot_uncertain");
        assert_eq!(view(&server).await, before);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(inspection(&server).await["charged_bytes"], charge);
        return;
    }
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["recovered"], true);
    assert_eq!(result["reader"], reader["id"]);
    assert_eq!(result["snapshot"], snapshot["id"]);
    let recovered = view(&server).await;
    assert_eq!(recovered["generation"], result["generation"]);
    assert_eq!(recovered["data"][0]["retained_readers"], json!([]));
    assert_eq!(recovered["data"][0]["sha256"], snapshot["sha256"]);
    assert_eq!(recovered["data"][0]["revision"], snapshot["revision"]);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("snapshots.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["entries"][snapshot["id"].as_str().unwrap()],
        saved_record
    );
    assert!(
        manifest
            .get("reader_intents")
            .is_none_or(|readers| readers.as_object().is_some_and(|rows| rows.is_empty()))
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(inspection(&server).await["charged_bytes"], charge);
    let (status, restored) = server.post("juiced/cache/restore", restore).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["restored"], true);
    assert_eq!(
        view(&server).await["data"][0]["retained_readers"],
        json!([])
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        server.event_count("fixture.one", "worker-startup", "started"),
        starts + 1
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn busy_or_cancelled_slot_cannot_be_published_under_a_previous_completed_session() {
    let server = cache_server(true, true).await;
    complete(&server, "finished-session", "completions").await;
    let generation = view(&server).await["generation"].clone();
    let socket = server
        .socket_request(
            "completions",
            json!({"model":"fixture.one","test_id":"cache-busy","fixture_delay_ms":2000}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "cache-busy", "started"))
        .await;
    let request = json!({"model":"fixture.one","slot":0,"session":"finished-session","expected_generation":generation,"maximum_bytes":1024});
    let (status, error) = server.post("juiced/cache/save", request.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "worker_busy");
    assert_eq!(view(&server).await["generation"], generation);
    drop(socket);
    server
        .wait_for(async || {
            server.event("fixture.one", "cache-busy", "cancelled")
                && server.get("juiced/state").await["resources"]["active_requests"] == 0
        })
        .await;
    let (status, error) = server.post("juiced/cache/save", request).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "cache_context_mismatch");
    assert_eq!(view(&server).await["generation"], generation);
    assert!(view(&server).await["data"].as_array().unwrap().is_empty());
    complete(&server, "finished-session", "completions").await;
    assert_eq!(save(&server, "finished-session").await["state"], "saved");
}

#[tokio::test]
async fn cache_requires_qualified_single_slot_profile_and_valid_session() {
    let server = Server::configured(true, |config| config["cache_enabled"] = json!(true)).await;
    let request = json!({"model":"fixture.one","slot":0,"session":"session","expected_generation":0,"maximum_bytes":1024});
    let (status, error) = server.post("juiced/cache/save", request).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(error["error"]["code"], "cache_profile_unsupported");
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    let server = cache_server(true, true).await;
    for session in ["", "invalid\nsession"] {
        let (status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":session,"expected_generation":0,"maximum_bytes":1024})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid_session");
    }
    let (status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"session","expected_generation":0,"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "worker_unloaded");
    let (status, error) = server
        .post(
            "juiced/cache/restore",
            json!({"model":"fixture.one","slot":1,"session":"session","snapshot":"unavailable"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(error["error"]["code"], "cache_profile_unsupported");
    assert_eq!(server.model("fixture.one").await["loaded"], false);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_native_launch_profile_cannot_restore_an_old_execution_snapshot() {
    let server = Server::configured(true, |config| {
        config["cache_enabled"] = json!(true);
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["options"] = json!({"parallel_slots":1,"context_tokens":512});
        model["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--slot-save-path"), json!("{cache}")]);
    })
    .await;
    complete(&server, "profile-bound-session", "completions").await;
    let snapshot = save(&server, "profile-bound-session").await;
    let generation = view(&server).await["generation"].clone();
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    let before = server.get("models/fixture.one/options").await;
    let (status,_)=server.post("models/fixture.one/options",json!({"expected_revision":before["revision"],"options":{"parallel_slots":1,"context_tokens":1024}})).await;
    assert_eq!(status, StatusCode::OK);
    let (status,error)=server.post("juiced/cache/restore",json!({"model":"fixture.one","slot":0,"session":"profile-bound-session","snapshot":snapshot["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "snapshot_identity_mismatch");
    assert_eq!(view(&server).await["generation"], generation);
    assert_eq!(view(&server).await["data"][0]["id"], snapshot["id"]);
    assert_eq!(
        complete(&server, "profile-bound-session", "completions").await["fixture_restores"],
        0
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn actual_cli_saves_inspects_restores_continues_and_forgets_native_slot_state() {
    use support::cli::call;
    let mut server = cache_server(true, false).await;
    let disabled = call(&server, &["cache"], None).await;
    let generation = disabled["configuration_generation"]
        .as_u64()
        .unwrap()
        .to_string();
    assert_eq!(
        call(
            &server,
            &["cache", "enable", "--generation", &generation],
            None
        )
        .await["enabled"],
        true
    );
    complete(&server, "cli-session", "completions").await;
    let cache = call(&server, &["cache"], None).await;
    let generation = cache["generation"].as_u64().unwrap().to_string();
    let saved = call(
        &server,
        &[
            "cache",
            "save",
            "--model",
            "fixture.one",
            "--session",
            "cli-session",
            "--generation",
            &generation,
            "--max-bytes",
            "1024",
            "--slot",
            "0",
        ],
        None,
    )
    .await;
    assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    let snapshot = saved["snapshot"]["id"].as_str().unwrap();
    let inspected = call(&server, &["cache", "inspect"], None).await;
    assert_eq!(inspected["read_only"], true);
    assert_eq!(inspected["recovery_authorized"], false);
    assert_eq!(inspected["snapshots"][0]["checksum_valid"], true);
    let page = call(&server, &["cache", "--limit", "1"], None).await;
    assert_eq!(page["data"][0]["id"], snapshot);
    assert_eq!(page["page"]["limit"], 1);
    let generation = page["generation"].as_u64().unwrap().to_string();
    let token = page["page"]["view"].as_str().unwrap();
    let end = call(
        &server,
        &[
            "cache",
            "--limit",
            "1",
            "--after",
            snapshot,
            "--generation",
            &generation,
            "--view",
            token,
        ],
        None,
    )
    .await;
    assert!(end["data"].as_array().unwrap().is_empty());
    let page = call(
        &server,
        &[
            "cache",
            "inspect",
            "--limit",
            "1",
            "--generation",
            &generation,
            "--view",
            token,
        ],
        None,
    )
    .await;
    assert_eq!(page["snapshots"][0]["checksum_valid"], true);
    assert_eq!(page["page"]["limit"], 1);
    assert_eq!(
        call(&server, &["unload", "fixture.one"], None).await["loaded"],
        false
    );
    server.restart().await;
    let restored = call(
        &server,
        &[
            "cache",
            "restore",
            snapshot,
            "--model",
            "fixture.one",
            "--session",
            "cli-session",
            "--slot",
            "0",
        ],
        None,
    )
    .await;
    assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
    assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
    assert_eq!(
        complete(&server, "cli-session", "completions").await["fixture_restores"],
        1
    );
    let revision = saved["snapshot"]["revision"].as_u64().unwrap().to_string();
    assert_eq!(
        call(
            &server,
            &[
                "cache",
                "forget",
                snapshot,
                "--session",
                "cli-session",
                "--revision",
                &revision
            ],
            None
        )
        .await["forgotten"],
        true
    );
    let cache = call(&server, &["cache"], None).await;
    assert_eq!(cache["data"], json!([]));
    let generation = cache["configuration_generation"]
        .as_u64()
        .unwrap()
        .to_string();
    assert_eq!(
        call(
            &server,
            &["cache", "disable", "--generation", &generation],
            None
        )
        .await["enabled"],
        false
    );
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn unsupported_runtime_observation_refuses_persistent_cache_without_staging_or_blocking_inference()
 {
    use support::cli::{failure, invoke};
    let server = cache_server(true, true).await;
    complete(&server, "unsupported-platform-session", "completions").await;
    let before = view(&server).await;
    let model = server.model("fixture.one").await;
    assert_eq!(model["runtime_identity"]["state"], "unknown");
    assert_eq!(
        model["runtime_identity"]["reason"],
        "runtime_identity_unsupported"
    );
    let (status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"unsupported-platform-session","expected_generation":before["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(error["error"]["code"], "runtime_identity_unsupported");
    assert_eq!(view(&server).await, before);
    let generation = before["generation"].as_u64().unwrap().to_string();
    let error = failure(
        invoke(
            Some(&server.base),
            &[
                "cache",
                "save",
                "--model",
                "fixture.one",
                "--session",
                "unsupported-platform-session",
                "--generation",
                &generation,
                "--max-bytes",
                "1024",
            ],
            &[("LEMONADE_ADMIN_API_KEY", server.admin_key().unwrap())],
            None,
        )
        .await,
        5,
    );
    assert_eq!(error["error"]["status"], 501);
    assert_eq!(error["error"]["code"], "runtime_identity_unsupported");
    assert_eq!(view(&server).await, before);
    assert_eq!(
        complete(&server, "unsupported-platform-session", "completions").await["model"],
        "fixture.one"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cache_pages_are_authenticated_generation_and_instance_bound_without_mutating_retained_data()
 {
    let mut server = cache_server(true, true).await;
    let mut expected = Vec::new();
    for session in ["page-session-one", "page-session-two", "page-session-three"] {
        complete(&server, session, "completions").await;
        expected.push(
            save(&server, session).await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    expected.sort();
    let cache_root = server.directory.path().join("cache");
    for index in [3, 1, 2] {
        std::fs::write(cache_root.join(format!("kv-observed-{index}.bin")), [1u8]).unwrap();
    }
    let retained = support::filesystem::tree(&cache_root);
    let config = std::fs::read(&server.config_path).unwrap();
    for prefix in PREFIXES {
        let path = format!("{prefix}/juiced/cache?limit=1");
        assert_eq!(
            server
                .json(Method::GET, &path, None, server.read_key())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, first) = server
            .json(Method::GET, &path, None, server.admin_key())
            .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["page"]["limit"], 1);
        let generation = first["generation"].as_u64().unwrap();
        let token = first["page"]["view"].as_str().unwrap();
        assert_eq!(token.len(), 64);
        let mut ids = vec![first["data"][0]["id"].as_str().unwrap().to_owned()];
        let mut next = first["page"]["next_after"].as_str().map(str::to_owned);
        while let Some(after) = next {
            let path = format!(
                "{prefix}/juiced/cache?limit=1&after={after}&expected_generation={generation}&expected_view={token}"
            );
            let (status, page) = server
                .json(Method::GET, &path, None, server.admin_key())
                .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["generation"], generation);
            assert_eq!(page["page"]["view"], token);
            assert_eq!(page["data"].as_array().unwrap().len(), 1);
            ids.push(page["data"][0]["id"].as_str().unwrap().to_owned());
            next = page["page"]["next_after"].as_str().map(str::to_owned);
        }
        assert_eq!(ids, expected);
        for suffix in [
            "limit=0".to_owned(),
            "limit=16385".to_owned(),
            "limit=one".to_owned(),
            "unknown=field".to_owned(),
            format!("after={}&expected_generation={generation}", expected[0]),
            format!(
                "orphan_after=kv-observed-1.bin&expected_generation={generation}&expected_view={token}"
            ),
        ] {
            let (status, _) = server
                .json(
                    Method::GET,
                    &format!("{prefix}/juiced/cache?{suffix}"),
                    None,
                    server.admin_key(),
                )
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{suffix}");
        }
        let (status, inspection) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/cache/inspect?limit=1"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{inspection}");
        assert_eq!(inspection["snapshots"].as_array().unwrap().len(), 1);
        assert_eq!(inspection["snapshots"][0]["checksum_valid"], true);
        assert_eq!(inspection["read_only"], true);
        assert_eq!(inspection["recovery_authorized"], false);
        assert_eq!(inspection["orphan_page"]["observed_count"], 3);
        assert_eq!(inspection["orphan_page"]["scan_complete"], true);
        assert_eq!(inspection["orphan_files"], json!(["kv-observed-1.bin"]));
        let (status, second) = server.json(Method::GET, &format!("{prefix}/juiced/cache/inspect?limit=1&orphan_after=kv-observed-1.bin&expected_generation={generation}&expected_view={token}"), None, server.admin_key()).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["orphan_files"], json!(["kv-observed-2.bin"]));
        assert_eq!(support::filesystem::tree(&cache_root), retained);
        assert_eq!(std::fs::read(&server.config_path).unwrap(), config);
    }
    let first = server
        .json(
            Method::GET,
            "/v1/juiced/cache?limit=1",
            None,
            server.admin_key(),
        )
        .await
        .1;
    let continuation = format!(
        "/v1/juiced/cache?limit=1&after={}&expected_generation={}&expected_view={}",
        first["page"]["next_after"].as_str().unwrap(),
        first["generation"],
        first["page"]["view"].as_str().unwrap()
    );
    server.restart().await;
    let (status, error) = server
        .json(Method::GET, &continuation, None, server.admin_key())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "cache_revision_conflict");
    assert_eq!(support::filesystem::tree(&cache_root), retained);
    assert!(server.model("fixture.one").await["pid"].is_null());
    let first = server
        .json(
            Method::GET,
            "/v1/juiced/cache?limit=1",
            None,
            server.admin_key(),
        )
        .await
        .1;
    complete(&server, "page-session-new", "completions").await;
    save(&server, "page-session-new").await;
    let continuation = format!(
        "/v1/juiced/cache?limit=1&after={}&expected_generation={}&expected_view={}",
        first["page"]["next_after"].as_str().unwrap(),
        first["generation"],
        first["page"]["view"].as_str().unwrap()
    );
    assert_eq!(
        server
            .json(Method::GET, &continuation, None, server.admin_key())
            .await
            .0,
        StatusCode::CONFLICT
    );
    server.rotate_credentials().await;
    let first = server
        .json(
            Method::GET,
            "/v1/juiced/cache?limit=1",
            None,
            server.admin_key(),
        )
        .await
        .1;
    assert_eq!(first["data"], json!([]));
    let continuation = format!(
        "/v1/juiced/cache?limit=1&after={}&expected_generation={}&expected_view={}",
        expected[0],
        first["generation"],
        first["page"]["view"].as_str().unwrap()
    );
    assert_eq!(
        server
            .json(Method::GET, &continuation, None, server.admin_key())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
