#![cfg(unix)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{path::Path, sync::atomic::Ordering, time::Duration};
use support::{
    Server,
    artifacts::Source,
    filesystem::{ReplacedRoot, digest, tree},
    rejected_manager,
};

struct PendingPost(tokio::task::JoinHandle<(StatusCode, Value)>);
impl PendingPost {
    fn load(server: &Server, model: &str) -> Self {
        let client = server.client.clone();
        let url = format!("{}/v1/load", server.base);
        let key = server.admin_key().unwrap();
        let model = model.to_owned();
        Self(tokio::spawn(async move {
            let response = client
                .post(url)
                .bearer_auth(key)
                .json(&json!({"model":model}))
                .send()
                .await
                .unwrap();
            (response.status(), response.json().await.unwrap())
        }))
    }
    async fn finish(mut self) -> (StatusCode, Value) {
        tokio::time::timeout(Duration::from_secs(3), &mut self.0)
            .await
            .expect("owned queued load did not settle")
            .unwrap()
    }
}
impl Drop for PendingPost {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn configuration(config: &mut Value) {
    config["cache_enabled"] = json!(true);
    config["artifact_source_policy"] = json!("allow-loopback-http");
    for model in config["models"].as_array_mut().unwrap() {
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["options"] =
            json!({"parallel_slots":1,"context_tokens":512,"threads":2,"gpu_layers":0});
        model["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--slot-save-path"), json!("{cache}")]);
    }
}
fn error(response: (StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(response.0, status, "{}", response.1);
    assert_eq!(response.1["error"]["code"], code);
}
async fn root_error(server: &Server, path: &str, request: Value) {
    error(
        server.post(path, request).await,
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_root_unavailable",
    );
}
async fn complete(server: &Server, id: &str) {
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", "root-scope")
        .json(&json!({"model":"fixture.one","prompt":"pipeline fixture","test_id":id}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _: Value = response.json().await.unwrap();
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
}
async fn job(server: &Server, id: &str) -> Value {
    server.get(&format!("juiced/artifacts/jobs/{id}")).await
}
async fn pull(server: &Server, source: &Source) -> String {
    let inventory = server.get("juiced/artifacts").await;
    let (status, result) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":inventory["generation"],"download":source.download()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result["id"].as_str().unwrap().to_owned()
}
async fn control(server: &Server, id: &str, action: &str) -> (StatusCode, Value) {
    let before = job(server, id).await;
    server
        .post(
            &format!("juiced/artifacts/jobs/{id}"),
            json!({"action":action,"expected_revision":before["revision"]}),
        )
        .await
}
async fn settled(server: &Server, id: &str) -> Value {
    server
        .wait_for(async || job(server, id).await["writer_active"] == false)
        .await;
    job(server, id).await
}
async fn graceful_shutdown(server: &mut Server) {
    assert_eq!(unsafe { libc::kill(server.pid() as i32, libc::SIGTERM) }, 0);
    let status = server.wait_for_manager_exit(Duration::from_secs(7)).await;
    server.retire_reaped_manager_without_group_signal();
    assert!(
        status.success(),
        "owned manager did not confirm shutdown: {status}"
    );
}
fn assert_exited(pid: u64) {
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "owned child remains after exit acknowledgement"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

#[tokio::test]
async fn all_roots_are_claimed_before_any_store_constructor_publishes_a_manifest() {
    let unrelated = Server::new(true).await;
    let candidate = Server::prepare(true, |_| {});
    let foreign = unrelated.directory.path().join("artifacts");
    let before = tree(&foreign);
    std::os::unix::fs::symlink(&foreign, candidate.directory.path().join("artifacts")).unwrap();
    rejected_manager(&candidate.config_path).await;
    assert!(
        !candidate
            .directory
            .path()
            .join("kv-cache/snapshots.json")
            .exists(),
        "cache opened before the conflicting artifact root was claimed"
    );
    assert_eq!(tree(&foreign), before);
    assert_eq!(
        unrelated
            .post(
                "completions",
                json!({"model":"fixture.one","test_id":"unrelated-bootstrap"})
            )
            .await
            .0,
        StatusCode::OK
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn replacement_of_any_claimed_root_blocks_cross_store_mutations_and_keeps_foreign_bytes_unchanged()
 {
    let source = Source::gated(0).await;
    let unrelated = Server::new(true).await;
    for root_name in ["config", "kv-cache", "artifacts"] {
        let mut server = Server::configured(true, configuration).await;
        complete(&server, "scope-baseline").await;
        let cache = server.get("juiced/cache").await;
        let (status,saved)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"root-scope","expected_generation":cache["generation"],"maximum_bytes":1024})).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
        let snapshot = saved["snapshot"].clone();
        let cache = server.get("juiced/cache").await;
        let artifacts = server.get("juiced/artifacts").await;
        let model = server.model("fixture.two").await;
        let pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
        let base = server.directory.path().to_owned();
        let path = if root_name == "config" {
            base.clone()
        } else {
            base.join(root_name)
        };
        let replacement = ReplacedRoot::replace(&path);
        let original_before = tree(replacement.original());
        let stores_before = if root_name == "config" {
            None
        } else {
            Some(tree(&base))
        };
        let revision = model["revision"].as_u64().unwrap();
        error(server.post("juiced/policy",json!({"model":"fixture.two","policy":"disabled","expected_revision":revision+1})).await,StatusCode::CONFLICT,"revision_conflict");
        error(server.post("juiced/artifacts/pull",json!({"expected_generation":artifacts["generation"].as_u64().unwrap()+1,"download":source.download()})).await,StatusCode::CONFLICT,"artifact_revision_conflict");
        error(server.post("juiced/artifacts/remove",json!({"expected_generation":artifacts["generation"],"artifact":"a".repeat(64)})).await,StatusCode::NOT_FOUND,"artifact_not_found");
        root_error(&server, "load", json!({"model":"fixture.two"})).await;
        root_error(
            &server,
            "completions",
            json!({"model":"fixture.one","test_id":"scope-must-not-dispatch"}),
        )
        .await;
        root_error(
            &server,
            "juiced/policy",
            json!({"model":"fixture.two","policy":"disabled","expected_revision":revision}),
        )
        .await;
        root_error(
            &server,
            "models/fixture.two/options",
            json!({"expected_revision":revision,"options":{"threads":1}}),
        )
        .await;
        let mut registration = server.config["models"][1].clone();
        registration["id"] = json!("root.new");
        registration["policy"] = json!("disabled");
        registration["revision"] = json!(0);
        root_error(&server, "models/register", registration).await;
        root_error(
            &server,
            "delete",
            json!({"model":"fixture.disabled","expected_revision":0}),
        )
        .await;
        root_error(
            &server,
            "juiced/cache",
            json!({"enabled":false,"expected_generation":cache["configuration_generation"]}),
        )
        .await;
        root_error(&server,"juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"root-scope","expected_generation":cache["generation"],"maximum_bytes":1024})).await;
        root_error(&server,"juiced/cache/restore",json!({"model":"fixture.one","slot":0,"session":"root-scope","snapshot":snapshot["id"]})).await;
        error(server.post("juiced/cache/forget",json!({"snapshot":snapshot["id"],"session":"root-scope","expected_revision":snapshot["revision"].as_u64().unwrap()+1})).await,StatusCode::CONFLICT,"cache_revision_conflict");
        error(
            server
                .post(
                    "juiced/cache/forget",
                    json!({"snapshot":"a".repeat(64),"session":"root-scope","expected_revision":0}),
                )
                .await,
            StatusCode::NOT_FOUND,
            "snapshot_not_found",
        );
        root_error(&server,"juiced/cache/forget",json!({"snapshot":snapshot["id"],"session":"root-scope","expected_revision":snapshot["revision"]})).await;
        root_error(
            &server,
            "juiced/artifacts/pull",
            json!({"expected_generation":artifacts["generation"],"download":source.download()}),
        )
        .await;
        assert_eq!(source.requests(), 0);
        assert_eq!(
            server.model("fixture.two").await["revision"],
            model["revision"]
        );
        assert!(server.model("fixture.two").await["pid"].is_null());
        let resources = server.get("juiced/state").await["resources"].clone();
        assert_eq!(resources["reserved_bytes"], 64);
        assert_eq!(resources["active_requests"], 0);
        assert_eq!(resources["power"]["active"], 0);
        assert_eq!(resources["power"]["attempts"], 0);
        assert_eq!(
            tree(replacement.original()),
            original_before,
            "{root_name}: rejected mutation wrote to retained old root"
        );
        replacement.assert_foreign_unchanged();
        if let Some(before) = stores_before {
            assert_eq!(
                tree(&base),
                before,
                "{root_name}: rejected mutation wrote to another store"
            );
        }
        assert_eq!(
            unrelated
                .post(
                    "completions",
                    json!({"model":"fixture.one","test_id":format!("unrelated-{root_name}")})
                )
                .await
                .0,
            StatusCode::OK
        );
        let (status, result) = server.post("unload", json!({"model":"fixture.one"})).await;
        assert!(
            status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
            "{result}"
        );
        if status == StatusCode::SERVICE_UNAVAILABLE {
            assert_eq!(result["error"]["code"], "storage_root_unavailable");
        }
        server
            .wait_for(async || server.get("juiced/state").await["resources"]["reserved_bytes"] == 0)
            .await;
        assert_exited(pid);
        graceful_shutdown(&mut server).await;
        replacement.assert_foreign_unchanged();
        server.stop();
    }
}

#[tokio::test]
async fn lost_root_rejects_waiting_startup_without_a_second_child_or_reservation() {
    let mut release = None;
    let mut server = Server::configured(true, |config| {
        config["startup_timeout_ms"] = json!(5000);
        let root = Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap();
        let path = root.join("startup-release");
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--startup-release-file"), json!(path)]);
        release = Some(path);
    })
    .await;
    let first = PendingPost::load(&server, "fixture.one");
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
    let second = PendingPost::load(&server, "fixture.two");
    server
        .wait_for(async || server.get("juiced/admission").await["startup"]["pending"] == 1)
        .await;
    let replacement = ReplacedRoot::replace(&server.directory.path().join("kv-cache"));
    let before = tree(replacement.original());
    std::fs::write(release.unwrap(), b"release after scope loss").unwrap();
    error(
        first.finish().await,
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_root_unavailable",
    );
    error(
        second.finish().await,
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_root_unavailable",
    );
    server
        .wait_for(async || {
            let view = server.get("juiced/admission").await;
            view["startup"]["active"] == 0
                && view["startup"]["pending"] == 0
                && view["pending_preparations"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        })
        .await;
    assert_exited(pid);
    assert!(!server.event("fixture.two", "worker-startup", "started"));
    assert!(server.model("fixture.two").await["pid"].is_null());
    let resources = server.get("juiced/state").await["resources"].clone();
    assert_eq!(resources["reserved_bytes"], 0);
    assert_eq!(resources["power"]["active"], 0);
    assert_eq!(resources["power"]["attempts"], 0);
    assert_eq!(tree(replacement.original()), before);
    replacement.assert_foreign_unchanged();
    graceful_shutdown(&mut server).await;
}

#[tokio::test]
async fn paused_range_resume_refuses_cross_root_loss_before_network_or_stage_mutation() {
    let source = Source::gated(4096).await;
    let mut server = Server::configured(true, configuration).await;
    let id = pull(&server, &source).await;
    server
        .wait_for(async || {
            job(&server, &id).await["downloaded_bytes"]
                .as_u64()
                .is_some_and(|n| n > 0)
        })
        .await;
    assert_eq!(control(&server, &id, "pause").await.0, StatusCode::OK);
    let paused = settled(&server, &id).await;
    assert_eq!(paused["state"], "paused");
    let active_id = pull(&server, &source).await;
    server
        .wait_for(async || {
            job(&server, &active_id).await["downloaded_bytes"]
                .as_u64()
                .is_some_and(|n| n > 0)
        })
        .await;
    assert_eq!(job(&server, &active_id).await["writer_active"], true);
    source.allow_through(8192);
    assert_eq!(control(&server, &id, "resume").await.0, StatusCode::OK);
    let part = server
        .directory
        .path()
        .join("artifacts")
        .join(format!("job-{id}.part"));
    let active_part = server
        .directory
        .path()
        .join("artifacts")
        .join(format!("job-{active_id}.part"));
    server
        .wait_for(async || {
            source.ranges.load(Ordering::Acquire) > 0
                && std::fs::metadata(&part).is_ok_and(|m| m.len() >= 8192)
                && std::fs::metadata(&active_part).is_ok_and(|m| m.len() >= 8192)
        })
        .await;
    assert_eq!(control(&server, &id, "pause").await.0, StatusCode::OK);
    let paused = settled(&server, &id).await;
    assert_eq!(paused["state"], "paused");
    let journal = server.get("juiced/artifacts").await;
    let stage_digest = digest(&part);
    let stage_len = std::fs::metadata(&part).unwrap().len();
    let requests = source.requests();
    let replacement = ReplacedRoot::replace(&server.directory.path().join("kv-cache"));
    let artifacts_before = tree(&server.directory.path().join("artifacts"));
    error(server.post(&format!("juiced/artifacts/jobs/{id}"),json!({"action":"resume","expected_revision":paused["revision"].as_u64().unwrap()+1})).await,StatusCode::CONFLICT,"artifact_revision_conflict");
    error(
        control(&server, &id, "resume").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_root_unavailable",
    );
    assert_eq!(source.requests(), requests);
    assert_eq!(digest(&part), stage_digest);
    assert_eq!(std::fs::metadata(&part).unwrap().len(), stage_len);
    assert_eq!(
        tree(&server.directory.path().join("artifacts")),
        artifacts_before
    );
    let after = server.get("juiced/artifacts").await;
    assert_eq!(after["charged_bytes"], journal["charged_bytes"]);
    assert!(after["charged_bytes"].as_u64().unwrap() > 0);
    assert_eq!(after["artifacts"], journal["artifacts"]);
    error(
        control(&server, &active_id, "cancel").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_root_unavailable",
    );
    let cancelled = settled(&server, &active_id).await;
    assert_ne!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["error"], "storage_root_unavailable");
    assert_eq!(
        server.get("juiced/artifacts").await["charged_bytes"],
        journal["charged_bytes"]
    );
    replacement.assert_foreign_unchanged();
    source.finish();
    graceful_shutdown(&mut server).await;
}

#[tokio::test]
async fn scope_loss_at_transfer_eof_retains_nonterminal_publication_charge_and_bytes_across_restart()
 {
    let source = Source::gated(usize::MAX).await;
    let mut server = Server::configured(true, configuration).await;
    let id = pull(&server, &source).await;
    let root = server.directory.path().join("artifacts");
    let part = root.join(format!("job-{id}.part"));
    server
        .wait_for(async || {
            std::fs::metadata(&part).is_ok_and(|m| m.len() == source.bytes.len() as u64)
        })
        .await;
    assert_eq!(
        digest(&part),
        source.download()["expected_sha256"].as_str().unwrap()
    );
    assert_eq!(job(&server, &id).await["writer_active"], true);
    let replacement = ReplacedRoot::replace(&server.directory.path().join("kv-cache"));
    source.finish();
    let failed = settled(&server, &id).await;
    assert_eq!(failed["state"], "interrupted");
    assert_eq!(failed["error"], "storage_root_unavailable");
    assert!(failed["artifact"].is_null());
    let inventory = server.get("juiced/artifacts").await;
    assert!(inventory["artifacts"].as_array().unwrap().is_empty());
    assert!(inventory["charged_bytes"].as_u64().unwrap() >= source.bytes.len() as u64);
    let journal = std::fs::read(root.join("artifacts.json")).unwrap();
    let hash = digest(&part);
    graceful_shutdown(&mut server).await;
    replacement.restore();
    server.start().await;
    let restarted = job(&server, &id).await;
    assert_eq!(restarted["state"], "interrupted");
    assert_eq!(restarted["writer_active"], false);
    assert_eq!(
        server.get("juiced/artifacts").await["charged_bytes"],
        inventory["charged_bytes"]
    );
    assert_eq!(digest(&part), hash);
    assert_eq!(std::fs::read(root.join("artifacts.json")).unwrap(), journal);
    assert!(
        server.get("juiced/artifacts").await["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
