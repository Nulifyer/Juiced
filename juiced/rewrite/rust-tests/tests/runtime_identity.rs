mod support;

use serde_json::{Value, json};
use std::collections::BTreeSet;
use support::Server;

fn summary(view: &Value, pid: &Value, root: &std::path::Path) {
    assert_eq!(
        view.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        [
            "state",
            "qualification",
            "closure_digest",
            "process_generation",
            "reason",
            "observed_at_unix_ms",
            "verified_at_unix_ms",
            "bundle_digest"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(view["bundle_digest"], Value::Null);
    let serialized = serde_json::to_string(view).unwrap();
    assert!(!serialized.contains(root.to_str().unwrap()));
    for forbidden in [
        "path",
        "paths",
        "model_file",
        "command",
        "files",
        "maps",
        "expected_executable",
    ] {
        assert!(view.get(forbidden).is_none());
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(view["state"], "observed");
        assert_eq!(view["qualification"], "runtime-file-observation-only");
        assert_eq!(view["reason"], Value::Null);
        assert!(view["observed_at_unix_ms"].as_u64().unwrap() > 0);
        assert!(
            view["verified_at_unix_ms"].as_u64().unwrap()
                >= view["observed_at_unix_ms"].as_u64().unwrap()
        );
        assert_eq!(view["closure_digest"].as_str().unwrap().len(), 64);
        let generation = &view["process_generation"];
        assert_eq!(generation["pid"], *pid);
        assert!(generation["start_ticks"].as_u64().unwrap() > 0);
        assert!(generation["executable_inode"].as_u64().unwrap() > 0);
        assert_eq!(
            generation
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            [
                "pid",
                "start_ticks",
                "executable_device",
                "executable_inode"
            ]
            .into_iter()
            .collect()
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        assert_eq!(view["state"], "unknown");
        assert_eq!(view["qualification"], "unqualified");
        assert_eq!(view["closure_digest"], Value::Null);
        assert_eq!(view["process_generation"], Value::Null);
        assert_eq!(view["reason"], "runtime_identity_unsupported");
    }
}

#[tokio::test]
async fn ready_runtime_summary_is_process_bound_redacted_and_distinct_from_execution_qualification()
{
    let server = Server::configured(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["options"] = json!({"parallel_slots":1,"context_tokens":512});
    })
    .await;
    assert_eq!(
        server.model("fixture.one").await["runtime_identity"],
        Value::Null
    );
    let (status, loaded) = server.post("load", json!({"model":"fixture.one"})).await;
    assert!(status.is_success());
    assert_eq!(loaded["state"], "ready");
    assert_eq!(loaded["loaded"], true);
    summary(
        &loaded["runtime_identity"],
        &loaded["pid"],
        server.config_path.parent().unwrap(),
    );
    assert_eq!(
        server.model("fixture.one").await["runtime_identity"],
        loaded["runtime_identity"]
    );
    let context = server.get("juiced/context?model=fixture.one").await;
    assert_eq!(
        context["applied"]["runtime_identity"],
        loaded["runtime_identity"]
    );
    assert_eq!(context["qualification"], "unqualified");
    assert_eq!(context["applied"]["engine_confirmed"], false);
    let (status, result) = server
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Runtime-bound pipeline"}),
        )
        .await;
    assert!(status.is_success());
    assert_eq!(result["model"], "fixture.one");
    assert!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0
            .is_success()
    );
    assert_eq!(
        server.model("fixture.one").await["runtime_identity"],
        Value::Null
    );
    let (status, reloaded) = server.post("load", json!({"model":"fixture.one"})).await;
    assert!(status.is_success());
    summary(
        &reloaded["runtime_identity"],
        &reloaded["pid"],
        server.config_path.parent().unwrap(),
    );
    assert_ne!(reloaded["pid"], loaded["pid"]);
    #[cfg(target_os = "linux")]
    {
        assert_ne!(
            reloaded["runtime_identity"]["process_generation"],
            loaded["runtime_identity"]["process_generation"]
        );
        assert_eq!(
            reloaded["runtime_identity"]["closure_digest"],
            loaded["runtime_identity"]["closure_digest"]
        );
    }
    let reloaded_context = server.get("juiced/context?model=fixture.one").await;
    assert_eq!(
        reloaded_context["applied"]["execution_fingerprint"],
        context["applied"]["execution_fingerprint"]
    );
}

#[cfg(target_os = "linux")]
struct MappedFixture {
    server: Server,
    copy: std::path::PathBuf,
    source: std::path::PathBuf,
    source_digest: String,
}
#[cfg(target_os = "linux")]
fn digest(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}
#[cfg(target_os = "linux")]
async fn mapped_fixture(delay: u64) -> MappedFixture {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let source = maps
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 6 || !fields[1].contains('x') {
                return None;
            }
            let path = std::path::PathBuf::from(fields[5..].join(" "));
            let name = path.file_name()?.to_str()?;
            let metadata = std::fs::metadata(&path).ok()?;
            (name.contains(".so")
                && metadata.is_file()
                && metadata.len() > 0
                && metadata.len() <= 16 * 1024 * 1024)
                .then_some(path)
        })
        .next()
        .expect("a small actual mapped native library is required");
    let source_digest = digest(&source);
    let mut copy = None;
    let server = Server::configured(true, |config| {
        config["cache_enabled"] = json!(true);
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["options"] = json!({"parallel_slots":1,"context_tokens":512});
        let path = std::path::Path::new(model["model_file"].as_str().unwrap())
            .parent()
            .unwrap()
            .join("private mapped library.so");
        assert_ne!(path, source);
        std::fs::copy(&source, &path).unwrap();
        model["args"].as_array_mut().unwrap().extend([
            json!("--slot-save-path"),
            json!("{cache}"),
            json!("--identity-map"),
            json!(path),
            json!("--slot-operation-delay-ms"),
            json!(delay.to_string()),
        ]);
        copy = Some(path);
    })
    .await;
    let copy = copy.unwrap();
    assert!(copy.starts_with(server.directory.path()));
    assert_eq!(digest(&copy), source_digest);
    MappedFixture {
        server,
        copy,
        source,
        source_digest,
    }
}
#[cfg(target_os = "linux")]
fn mutate_owned_copy(fixture: &MappedFixture) {
    use std::io::{Read, Seek, SeekFrom, Write};
    assert!(fixture.copy.starts_with(fixture.server.directory.path()));
    assert_ne!(fixture.copy, fixture.source);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fixture.copy)
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 1;
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    assert_eq!(digest(&fixture.source), fixture.source_digest);
    assert_ne!(digest(&fixture.copy), fixture.source_digest);
}
#[cfg(target_os = "linux")]
async fn completed_session(server: &Server) {
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", "runtime-session")
        .json(&json!({"model":"fixture.one","prompt":"Runtime file binding pipeline"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.bytes().await.unwrap();
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
}
#[cfg(target_os = "linux")]
async fn cache(server: &Server) -> Value {
    server
        .json(
            reqwest::Method::GET,
            "/v1/juiced/cache",
            None,
            server.admin_key(),
        )
        .await
        .1
}
#[cfg(target_os = "linux")]
fn save_body(generation: &Value) -> Value {
    json!({"model":"fixture.one","slot":0,"session":"runtime-session","expected_generation":generation,"maximum_bytes":1024})
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_owned_mapped_module_refuses_cache_before_engine_dispatch_and_preserves_snapshot() {
    let fixture = mapped_fixture(0).await;
    let server = &fixture.server;
    completed_session(server).await;
    let (status, saved) = server
        .post(
            "juiced/cache/save",
            save_body(&cache(server).await["generation"]),
        )
        .await;
    assert!(status.is_success(), "{saved}");
    let before = cache(server).await;
    let ready = server.model("fixture.one").await;
    let saved_path = server.directory.path().join("kv-cache").join(format!(
        "kv-{}.bin",
        saved["snapshot"]["id"].as_str().unwrap()
    ));
    let bytes = std::fs::read(&saved_path).unwrap();
    let save_calls = server.event_count("fixture.one", "slot-save", "started");
    assert_eq!(save_calls, 1);
    mutate_owned_copy(&fixture);
    let (status,error)=server.post("juiced/cache/restore",json!({"model":"fixture.one","slot":0,"session":"runtime-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "runtime_identity_changed");
    assert!(!server.event("fixture.one", "slot-restore", "started"));
    let (status, error) = server
        .post("juiced/cache/save", save_body(&before["generation"]))
        .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "runtime_identity_changed");
    assert_eq!(
        server.event_count("fixture.one", "slot-save", "started"),
        save_calls
    );
    assert_eq!(cache(server).await, before);
    assert_eq!(std::fs::read(&saved_path).unwrap(), bytes);
    let stale = server.model("fixture.one").await;
    assert_eq!(stale["pid"], ready["pid"]);
    assert_eq!(stale["runtime_identity"]["state"], "stale");
    assert_eq!(
        stale["runtime_identity"]["reason"],
        "runtime_identity_changed"
    );
    for field in [
        "closure_digest",
        "process_generation",
        "observed_at_unix_ms",
        "verified_at_unix_ms",
    ] {
        assert_eq!(
            stale["runtime_identity"][field],
            ready["runtime_identity"][field]
        );
    }
    assert!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0
            .is_success()
    );
    assert!(
        server
            .post("load", json!({"model":"fixture.one"}))
            .await
            .0
            .is_success()
    );
    let new = server.model("fixture.one").await;
    assert_eq!(new["runtime_identity"]["state"], "observed");
    assert_ne!(
        new["runtime_identity"]["closure_digest"],
        ready["runtime_identity"]["closure_digest"]
    );
    let (status,error)=server.post("juiced/cache/restore",json!({"model":"fixture.one","slot":0,"session":"runtime-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "snapshot_identity_mismatch");
    assert_eq!(cache(server).await, before);
    assert_eq!(digest(&fixture.source), fixture.source_digest);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_owned_mapped_module_during_engine_save_retains_uncertain_bytes_and_charge() {
    let fixture = mapped_fixture(500).await;
    let server = &fixture.server;
    completed_session(server).await;
    let before = cache(server).await;
    let client = server.client.clone();
    let url = format!("{}/v1/juiced/cache/save", server.base);
    let body = save_body(&before["generation"]);
    let key = server.admin_key().unwrap();
    let saving = tokio::spawn(async move {
        let response = client
            .post(url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .unwrap();
        (response.status(), response.json::<Value>().await.unwrap())
    });
    server
        .wait_for(async || server.event("fixture.one", "slot-save", "started"))
        .await;
    mutate_owned_copy(&fixture);
    let (status, error) = saving.await.unwrap();
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "runtime_identity_changed");
    let inspected = server
        .json(
            reqwest::Method::GET,
            "/v1/juiced/cache/inspect",
            None,
            server.admin_key(),
        )
        .await
        .1;
    assert_eq!(inspected["snapshots"].as_array().unwrap().len(), 1);
    let snapshot = &inspected["snapshots"][0]["snapshot"];
    assert_eq!(snapshot["state"], "uncertain");
    assert!(inspected["charged_bytes"].as_u64().unwrap() >= 1024);
    assert_eq!(inspected["read_only"], true);
    assert_eq!(inspected["recovery_authorized"], false);
    let path = server
        .directory
        .path()
        .join("kv-cache")
        .join(format!("kv-{}.bin", snapshot["id"].as_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.is_empty());
    let (status,error)=server.post("juiced/cache/forget",json!({"snapshot":snapshot["id"],"session":"runtime-session","expected_revision":snapshot["revision"]})).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "snapshot_uncertain");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        server.model("fixture.one").await["runtime_identity"]["state"],
        "stale"
    );
    assert_eq!(digest(&fixture.source), fixture.source_digest);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn changed_owned_mapped_module_during_engine_restore_keeps_snapshot_and_drops_context_ownership()
 {
    let fixture = mapped_fixture(500).await;
    let server = &fixture.server;
    completed_session(server).await;
    let (status, saved) = server
        .post(
            "juiced/cache/save",
            save_body(&cache(server).await["generation"]),
        )
        .await;
    assert!(status.is_success(), "{saved}");
    let before = cache(server).await;
    let ready = server.model("fixture.one").await;
    let path = server.directory.path().join("kv-cache").join(format!(
        "kv-{}.bin",
        saved["snapshot"]["id"].as_str().unwrap()
    ));
    let bytes = std::fs::read(&path).unwrap();
    let client = server.client.clone();
    let url = format!("{}/v1/juiced/cache/restore", server.base);
    let key = server.admin_key().unwrap();
    let body = json!({"model":"fixture.one","slot":0,"session":"runtime-session","snapshot":saved["snapshot"]["id"]});
    let restoring = tokio::spawn(async move {
        let response = client
            .post(url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .unwrap();
        (response.status(), response.json::<Value>().await.unwrap())
    });
    server
        .wait_for(async || server.event("fixture.one", "slot-restore", "started"))
        .await;
    mutate_owned_copy(&fixture);
    let (status, error) = restoring.await.unwrap();
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "runtime_identity_changed");
    let after = cache(server).await;
    assert!(after["generation"].as_u64().unwrap() > before["generation"].as_u64().unwrap());
    assert_ne!(after["page"]["view"], before["page"]["view"]);
    let mut expected = before.clone();
    expected["generation"] = after["generation"].clone();
    expected["page"]["view"] = after["page"]["view"].clone();
    assert_eq!(after, expected);
    assert_eq!(after["data"][0]["retained_readers"], json!([]));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let stale = server.model("fixture.one").await;
    assert_eq!(stale["pid"], ready["pid"]);
    assert_eq!(stale["runtime_identity"]["state"], "stale");
    assert_eq!(
        stale["runtime_identity"]["reason"],
        "runtime_identity_changed"
    );
    let (status, error) = server
        .post("juiced/cache/save", save_body(&before["generation"]))
        .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "cache_context_mismatch");
    let (status, continued) = server
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Observe actual slot restore count only"}),
        )
        .await;
    assert!(status.is_success());
    assert_eq!(continued["fixture_restores"], 1);
    assert_eq!(server.model("fixture.one").await["pid"], ready["pid"]);
    assert_eq!(digest(&fixture.source), fixture.source_digest);
}
