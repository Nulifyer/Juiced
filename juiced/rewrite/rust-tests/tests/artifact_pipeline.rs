mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use support::{PREFIXES, Server, artifacts::Source};

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
async fn job(server: &Server, id: &str) -> Value {
    server.get(&format!("juiced/artifacts/jobs/{id}")).await
}
async fn wait_state(server: &Server, id: &str, states: &[&str]) -> Value {
    server
        .wait_for(async || states.contains(&job(server, id).await["state"].as_str().unwrap()))
        .await;
    job(server, id).await
}
async fn pull(server: &Server, source: &Source, prefix: &str) -> Value {
    let generation = inventory(server).await["generation"].clone();
    let (status, result) = server
        .json(
            Method::POST,
            &format!("{prefix}/juiced/artifacts/pull"),
            Some(json!({"expected_generation":generation,"download":source.download()})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}
async fn control(server: &Server, id: &str, action: &str, prefix: &str) -> Value {
    let revision = job(server, id).await["revision"].clone();
    let (status, result) = server
        .json(
            Method::POST,
            &format!("{prefix}/juiced/artifacts/jobs/{id}"),
            Some(json!({"expected_revision":revision,"action":action})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{action}: {result}");
    result
}

#[tokio::test]
async fn verified_chunked_download_inventory_and_removal_work_under_every_prefix() {
    let source = Source::start().await;
    let server = server().await;
    for prefix in PREFIXES {
        let started = pull(&server, &source, prefix).await;
        let id = started["id"].as_str().unwrap();
        let done = wait_state(&server, id, &["complete"]).await;
        assert_eq!(done["downloaded_bytes"], source.bytes.len());
        let (status, observed) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/artifacts/jobs/{id}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(observed["artifact"], done["artifact"]);
        let before = inventory(&server).await;
        assert_eq!(before["read_only"], true);
        assert_eq!(before["recovery_authorized"], false);
        let artifact = before["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|artifact| artifact["id"] == done["artifact"])
            .unwrap();
        assert_eq!(artifact["sha256"], source.download()["expected_sha256"]);
        assert_eq!(artifact["bytes"], source.bytes.len());
        let (status, result) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/artifacts/remove"),
                Some(
                    json!({"artifact":done["artifact"],"expected_generation":before["generation"]}),
                ),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["removed"], true);
        assert!(
            inventory(&server).await["artifacts"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn pause_resume_cancel_and_restart_preserve_verified_transfer_state() {
    let source = Source::start().await;
    let mut server = server().await;
    let started = pull(&server, &source, "/v1").await;
    let id = started["id"].as_str().unwrap();
    server
        .wait_for(async || job(&server, id).await["downloaded_bytes"].as_u64().unwrap() > 0)
        .await;
    control(&server, id, "pause", "/api/v0").await;
    let paused = wait_state(&server, id, &["paused"]).await;
    assert!(paused["downloaded_bytes"].as_u64().unwrap() > 0);
    assert_eq!(paused["writer_active"], false);
    let (status, stale) = server
        .post(
            &format!("juiced/artifacts/jobs/{id}"),
            json!({"action":"resume","expected_revision":0}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "artifact_revision_conflict");
    server.restart().await;
    assert_eq!(job(&server, id).await["state"], "paused");
    control(&server, id, "resume", "/api/v1").await;
    let done = wait_state(&server, id, &["complete"]).await;
    assert_eq!(done["downloaded_bytes"], source.bytes.len());
    assert!(source.ranges.load(Ordering::Acquire) > 0);
    let second = pull(&server, &source, "/v0").await;
    let second_id = second["id"].as_str().unwrap();
    control(&server, second_id, "cancel", "/v0").await;
    assert_eq!(
        wait_state(&server, second_id, &["cancelled"]).await["writer_active"],
        false
    );
    assert_eq!(
        inventory(&server).await["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn bad_hash_is_quarantined_retry_remains_bounded_and_cas_is_enforced() {
    let source = Source::start().await;
    let server = server().await;
    let mut download = source.download();
    download["expected_sha256"] = json!("0".repeat(64));
    let generation = inventory(&server).await["generation"].clone();
    let (status, started) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":download}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = started["id"].as_str().unwrap();
    let quarantined = wait_state(&server, id, &["quarantined"]).await;
    assert_eq!(quarantined["error"], "artifact_hash_mismatch");
    assert!(
        inventory(&server).await["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, _) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":source.download()}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    control(&server, id, "retry", "/v1").await;
    let failed = wait_state(&server, id, &["quarantined"]).await;
    assert!(failed["revision"].as_u64().unwrap() > quarantined["revision"].as_u64().unwrap());
}

#[tokio::test]
async fn source_policies_reject_ssrf_secrets_and_offline_network() {
    let source = Source::start().await;
    for policy in ["public-https", "offline", "allow-loopback-http"] {
        let server = Server::configured(true, |config| {
            config["artifact_source_policy"] = json!(policy)
        })
        .await;
        let before = inventory(&server).await;
        let urls = if policy == "allow-loopback-http" {
            vec![
                "http://169.254.169.254/latest/meta-data".to_owned(),
                format!(
                    "http://user:credential-value-private@{}/weights",
                    source.base.trim_start_matches("http://")
                ),
                format!("{}/weights?token=credential-value-private", source.base),
            ]
        } else {
            vec![format!("{}/weights", source.base)]
        };
        for url in urls {
            let mut download = source.download();
            download["source_url"] = json!(url);
            let (status, result) = server
                .post(
                    "juiced/artifacts/pull",
                    json!({"expected_generation":before["generation"],"download":download}),
                )
                .await;
            assert!(status.is_client_error(), "{policy}: {result}");
            assert!(!result.to_string().contains("credential-value-private"));
        }
        assert_eq!(inventory(&server).await["generation"], before["generation"]);
        assert!(
            inventory(&server).await["jobs"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn interrupted_transport_retries_from_start_and_publishes_verified_bytes() {
    let source = Source::start().await;
    let server = server().await;
    let mut download = source.download();
    download["source_url"] = json!(format!("{}/fail-once", source.base));
    let generation = inventory(&server).await["generation"].clone();
    let (status, started) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":download}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = started["id"].as_str().unwrap();
    let failed = wait_state(&server, id, &["failed"]).await;
    assert_eq!(failed["error"], "artifact_network");
    assert!(
        inventory(&server).await["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    control(&server, id, "retry", "/v1").await;
    let done = wait_state(&server, id, &["complete"]).await;
    assert_eq!(done["downloaded_bytes"], source.bytes.len());
    assert_eq!(
        inventory(&server).await["artifacts"][0]["sha256"],
        source.download()["expected_sha256"]
    );
}

#[tokio::test]
async fn invalid_download_bounds_hash_and_stale_removal_do_not_mutate_storage() {
    let source = Source::start().await;
    let server = server().await;
    let before = inventory(&server).await;
    for (field, value) in [
        ("maximum_bytes", json!(0)),
        ("expected_bytes", json!(source.bytes.len() + 1)),
        ("expected_sha256", json!("invalid")),
    ] {
        let mut download = source.download();
        download[field] = value;
        let (status, error) = server
            .post(
                "juiced/artifacts/pull",
                json!({"expected_generation":before["generation"],"download":download}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(inventory(&server).await["generation"], before["generation"]);
    }
    let started = pull(&server, &source, "/v1").await;
    let id = started["id"].as_str().unwrap();
    let done = wait_state(&server, id, &["complete"]).await;
    let (status, error) = server
        .post(
            "juiced/artifacts/remove",
            json!({"artifact":done["artifact"],"expected_generation":before["generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "artifact_revision_conflict");
    assert_eq!(
        inventory(&server).await["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn acquired_artifact_registration_worker_hold_and_restart_guard_shared_bytes_until_delete() {
    let source = Source::start().await;
    let mut server = server().await;
    let started = pull(&server, &source, "/v1").await;
    let job_id = started["id"].as_str().unwrap();
    let done = wait_state(&server, job_id, &["complete"]).await;
    let artifact = done["artifact"].clone();
    let mut registration = server.config["models"][0].clone();
    registration["id"] = json!("managed-artifact-model");
    registration["model_file"] = json!("");
    registration["artifact"] = artifact.clone();
    registration["policy"] = json!("disabled");
    registration["revision"] = json!(0);
    let (status, registered) = server.post("models/register", registration.clone()).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let held = inventory(&server).await;
    let entry = &held["artifacts"][0];
    assert!(
        entry["references"]
            .as_array()
            .unwrap()
            .contains(&json!("model:managed-artifact-model"))
    );
    assert!(entry["active_holds"].as_u64().unwrap() > 0);
    server
        .wait_for(async || job(&server, job_id).await["writer_active"] == false)
        .await;
    let completed = job(&server, job_id).await;
    let held = inventory(&server).await;
    let artifact_path = server
        .config_path
        .parent()
        .unwrap()
        .join("artifacts")
        .join(format!("artifact-{}.bin", artifact.as_str().unwrap()));
    let published = std::fs::read(&artifact_path).unwrap();
    assert_eq!(published.as_slice(), source.bytes.as_slice());
    let (status,error)=server.post(&format!("juiced/artifacts/jobs/{job_id}"),json!({"action":"forget","expected_revision":completed["revision"].as_u64().unwrap()+1})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "artifact_revision_conflict");
    assert_eq!(inventory(&server).await, held);
    let forgotten = control(&server, job_id, "forget", "/v1").await;
    assert_eq!(
        forgotten,
        json!({"forgotten":true,"job":job_id,"artifacts_preserved":true})
    );
    assert_job_gone(&server, job_id).await;
    let after_forget = inventory(&server).await;
    assert_eq!(after_forget["artifacts"], held["artifacts"]);
    assert_eq!(after_forget["charged_bytes"], held["charged_bytes"]);
    assert!(after_forget["jobs"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read(&artifact_path).unwrap(), published);
    let mut additional = registration.clone();
    additional["id"] = json!("after-forget-model");
    let (status, additional) = server.post("models/register", additional).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inventory(&server).await["artifacts"][0]["active_holds"], 2);
    let (status, _) = server
        .post(
            "delete",
            json!({"model":"after-forget-model","expected_revision":additional["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let held = inventory(&server).await;
    let (status, error) = server
        .post(
            "juiced/artifacts/remove",
            json!({"artifact":artifact,"expected_generation":held["generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "artifact_busy");
    let (status,_)=server.post("juiced/policy",json!({"model":"managed-artifact-model","policy":"on-demand","expected_revision":registered["revision"]})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = server
        .post("load", json!({"model":"managed-artifact-model"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, output) = server
        .post(
            "completions",
            json!({"model":"managed-artifact-model","prompt":"Pipeline"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(output["model"], "managed-artifact-model");
    server.restart().await;
    assert_job_gone(&server, job_id).await;
    assert_eq!(std::fs::read(&artifact_path).unwrap(), published);
    let (status, _) = server
        .post("load", json!({"model":"managed-artifact-model"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        inventory(&server).await["artifacts"][0]["active_holds"]
            .as_u64()
            .unwrap()
            > 0
    );
    let (status, _) = server
        .post("unload", json!({"model":"managed-artifact-model"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let current = server.model("managed-artifact-model").await;
    let (status,disabled)=server.post("juiced/policy",json!({"model":"managed-artifact-model","policy":"disabled","expected_revision":current["revision"]})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":"managed-artifact-model","expected_revision":disabled["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["weights_retained"], true);
    assert_eq!(removed["artifact_reference_retained"], false);
    let released = inventory(&server).await;
    assert!(
        released["artifacts"][0]["references"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(released["artifacts"][0]["active_holds"], 0);
    let (status, removed) = server
        .post(
            "juiced/artifacts/remove",
            json!({"artifact":artifact,"expected_generation":released["generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert!(
        inventory(&server).await["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

async fn assert_job_gone(server: &Server, id: &str) {
    let (status, error) = server
        .json(
            Method::GET,
            &format!("/v1/juiced/artifacts/jobs/{id}"),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "artifact_not_found");
}

#[tokio::test]
async fn forgetting_cancelled_jobs_removes_only_terminal_history_under_all_prefixes() {
    let source = Source::start().await;
    let mut server = server().await;
    for prefix in PREFIXES {
        let started = pull(&server, &source, prefix).await;
        let id = started["id"].as_str().unwrap();
        server
            .wait_for(async || job(&server, id).await["downloaded_bytes"].as_u64().unwrap() > 0)
            .await;
        let current = job(&server, id).await;
        let (status, error) = server
            .post(
                &format!("juiced/artifacts/jobs/{id}"),
                json!({"action":"forget","expected_revision":current["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "artifact_busy");
        control(&server, id, "pause", prefix).await;
        server
            .wait_for(async || {
                let job = job(&server, id).await;
                job["state"] == "paused" && job["writer_active"] == false
            })
            .await;
        let paused = job(&server, id).await;
        let before = inventory(&server).await;
        assert!(before["charged_bytes"].as_u64().unwrap() > 0);
        let part = server
            .config_path
            .parent()
            .unwrap()
            .join("artifacts")
            .join(format!("job-{id}.part"));
        let retained = std::fs::read(&part).unwrap();
        assert!(!retained.is_empty());
        let (status, error) = server
            .post(
                &format!("juiced/artifacts/jobs/{id}"),
                json!({"action":"forget","expected_revision":paused["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "artifact_revision_conflict");
        assert_eq!(inventory(&server).await, before);
        assert_eq!(std::fs::read(&part).unwrap(), retained);
        control(&server, id, "cancel", prefix).await;
        server
            .wait_for(async || {
                let job = job(&server, id).await;
                job["state"] == "cancelled" && job["writer_active"] == false
            })
            .await;
        assert_eq!(inventory(&server).await["charged_bytes"], 0);
        assert!(!part.exists());
        let forgotten = control(&server, id, "forget", prefix).await;
        assert_eq!(
            forgotten,
            json!({"forgotten":true,"job":id,"artifacts_preserved":true})
        );
        assert_job_gone(&server, id).await;
        let after = inventory(&server).await;
        assert_eq!(after["charged_bytes"], 0);
        assert!(after["jobs"].as_array().unwrap().is_empty());
        assert!(after["artifacts"].as_array().unwrap().is_empty());
    }
    server.restart().await;
    let after = inventory(&server).await;
    assert_eq!(after["charged_bytes"], 0);
    assert!(after["jobs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn failed_and_quarantined_transfers_cannot_be_forgotten_or_release_unconfirmed_charges() {
    let source = Source::start().await;
    let mut server = server().await;
    let mut ids = Vec::new();
    for state in ["failed", "quarantined"] {
        let mut download = source.download();
        if state == "failed" {
            download["source_url"] = json!(format!("{}/fail-once", source.base));
        } else {
            download["expected_sha256"] = json!("0".repeat(64));
        }
        let generation = inventory(&server).await["generation"].clone();
        let (status, started) = server
            .post(
                "juiced/artifacts/pull",
                json!({"expected_generation":generation,"download":download}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let id = started["id"].as_str().unwrap();
        server
            .wait_for(async || {
                let job = job(&server, id).await;
                job["state"] == state && job["writer_active"] == false
            })
            .await;
        let current = job(&server, id).await;
        let before = inventory(&server).await;
        let part = server
            .config_path
            .parent()
            .unwrap()
            .join("artifacts")
            .join(format!("job-{id}.part"));
        let bytes = std::fs::read(&part).unwrap();
        assert!(!bytes.is_empty());
        assert!(before["charged_bytes"].as_u64().unwrap() >= source.bytes.len() as u64);
        let (status, error) = server
            .post(
                &format!("juiced/artifacts/jobs/{id}"),
                json!({"action":"forget","expected_revision":current["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "artifact_revision_conflict");
        assert_eq!(inventory(&server).await, before);
        assert_eq!(std::fs::read(&part).unwrap(), bytes);
        ids.push((id.to_owned(), current, bytes));
    }
    let before = inventory(&server).await;
    server.restart().await;
    assert_eq!(inventory(&server).await, before);
    for (id, current, bytes) in ids {
        let (status, _) = server
            .post(
                &format!("juiced/artifacts/jobs/{id}"),
                json!({"action":"forget","expected_revision":current["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let part = server
            .config_path
            .parent()
            .unwrap()
            .join("artifacts")
            .join(format!("job-{id}.part"));
        assert_eq!(std::fs::read(&part).unwrap(), bytes);
        let cancelled = control(&server, &id, "cancel", "/v1").await;
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(cancelled["writer_active"], false);
        assert!(!part.exists());
        let forgotten = control(&server, &id, "forget", "/v1").await;
        assert_eq!(forgotten["forgotten"], true);
        assert_job_gone(&server, &id).await;
    }
    let cleaned = inventory(&server).await;
    assert_eq!(cleaned["charged_bytes"], 0);
    assert!(cleaned["jobs"].as_array().unwrap().is_empty());
    assert!(cleaned["artifacts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn actual_cli_acquires_controls_inspects_and_forgets_a_verified_artifact_job() {
    use support::cli::call;
    let source = Source::start().await;
    let server = server().await;
    let before = call(&server, &["artifacts"], None).await;
    let generation = before["generation"].as_u64().unwrap().to_string();
    let input = serde_json::to_vec(&source.download()).unwrap();
    let started = call(
        &server,
        &["pull", "--file", "-", "--generation", &generation],
        Some(&input),
    )
    .await;
    let id = started["id"].as_str().unwrap();
    server
        .wait_for(async || job(&server, id).await["downloaded_bytes"].as_u64().unwrap() > 0)
        .await;
    let running = call(&server, &["job", id], None).await;
    let revision = running["revision"].as_u64().unwrap().to_string();
    call(
        &server,
        &["job", id, "pause", "--revision", &revision],
        None,
    )
    .await;
    server
        .wait_for(async || {
            let job = job(&server, id).await;
            job["state"] == "paused" && job["writer_active"] == false
        })
        .await;
    let paused = call(&server, &["job", id], None).await;
    let revision = paused["revision"].as_u64().unwrap().to_string();
    call(
        &server,
        &["job", id, "resume", "--revision", &revision],
        None,
    )
    .await;
    server
        .wait_for(async || {
            let job = job(&server, id).await;
            job["state"] == "complete" && job["writer_active"] == false
        })
        .await;
    let complete = call(&server, &["job", id], None).await;
    assert_eq!(complete["downloaded_bytes"], source.bytes.len());
    let revision = complete["revision"].as_u64().unwrap().to_string();
    let forgotten = call(
        &server,
        &["job", id, "forget", "--revision", &revision],
        None,
    )
    .await;
    assert_eq!(forgotten["forgotten"], true);
    assert_eq!(forgotten["artifacts_preserved"], true);
    assert_job_gone(&server, id).await;
    let artifacts = call(&server, &["artifacts"], None).await;
    assert_eq!(
        artifacts["artifacts"][0]["sha256"],
        source.download()["expected_sha256"]
    );
    let generation = artifacts["generation"].as_u64().unwrap().to_string();
    let removed = call(
        &server,
        &[
            "artifacts",
            "remove",
            complete["artifact"].as_str().unwrap(),
            "--generation",
            &generation,
        ],
        None,
    )
    .await;
    assert_eq!(removed["removed"], true);
    assert_eq!(
        call(&server, &["artifacts"], None).await["charged_bytes"],
        0
    );
}

#[tokio::test]
async fn acquisition_interrupted_by_manager_crash_requires_explicit_cancel_before_forgetting() {
    let source = Source::start().await;
    let mut server = server().await;
    let started = pull(&server, &source, "/v1").await;
    let id = started["id"].as_str().unwrap();
    server
        .wait_for(async || job(&server, id).await["downloaded_bytes"].as_u64().unwrap() > 0)
        .await;
    server.crash();
    let root = server.config_path.parent().unwrap().join("artifacts");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("artifacts.json")).unwrap()).unwrap();
    assert_eq!(manifest["jobs"][id]["state"], "downloading");
    let part = root.join(format!("job-{id}.part"));
    assert!(part.is_file());
    server.start().await;
    let interrupted = job(&server, id).await;
    assert_eq!(interrupted["state"], "interrupted");
    assert_eq!(interrupted["writer_active"], false);
    assert!(inventory(&server).await["charged_bytes"].as_u64().unwrap() > 0);
    let (status, error) = server
        .post(
            &format!("juiced/artifacts/jobs/{id}"),
            json!({"action":"forget","expected_revision":interrupted["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "artifact_revision_conflict");
    assert!(part.is_file());
    assert_eq!(
        control(&server, id, "cancel", "/v1").await["state"],
        "cancelled"
    );
    assert!(!part.exists());
    assert_eq!(inventory(&server).await["charged_bytes"], 0);
    assert_eq!(
        control(&server, id, "forget", "/v1").await["forgotten"],
        true
    );
    assert_job_gone(&server, id).await;
}

#[tokio::test]
async fn ambiguous_publication_journal_never_authorizes_cancel_or_forget_of_published_bytes() {
    let source = Source::start().await;
    let mut server = server().await;
    let started = pull(&server, &source, "/v1").await;
    let id = started["id"].as_str().unwrap();
    server
        .wait_for(async || {
            let job = job(&server, id).await;
            job["state"] == "complete" && job["writer_active"] == false
        })
        .await;
    let completed = job(&server, id).await;
    let artifact = completed["artifact"].as_str().unwrap();
    server.stop();
    let root = server.config_path.parent().unwrap().join("artifacts");
    let path = root.join("artifacts.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["jobs"][id]["state"] = json!("publishing");
    manifest["jobs"][id]["artifact"] = Value::Null;
    manifest["artifacts"]
        .as_object_mut()
        .unwrap()
        .remove(artifact);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let target = root.join(format!("artifact-{artifact}.bin"));
    let published = std::fs::read(&target).unwrap();
    assert_eq!(published.as_slice(), source.bytes.as_slice());
    server.start().await;
    let interrupted = job(&server, id).await;
    assert_eq!(interrupted["state"], "interrupted");
    assert_eq!(interrupted["error"], "artifact_publication_uncertain");
    assert_eq!(interrupted["writer_active"], false);
    let before = inventory(&server).await;
    let journal = std::fs::read(&path).unwrap();
    assert!(before["charged_bytes"].as_u64().unwrap() > 0);
    for action in ["cancel", "forget"] {
        let (status, error) = server
            .post(
                &format!("juiced/artifacts/jobs/{id}"),
                json!({"action":action,"expected_revision":interrupted["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{action}");
        assert_eq!(error["error"]["code"], "artifact_revision_conflict");
        assert_eq!(inventory(&server).await, before);
        assert_eq!(std::fs::read(&path).unwrap(), journal);
        assert_eq!(std::fs::read(&target).unwrap(), published);
    }
}

#[tokio::test]
async fn managed_artifact_tamper_before_load_rejects_spawn_and_releases_reservation() {
    let source = Source::start().await;
    let server = server().await;
    let started = pull(&server, &source, "/v1").await;
    let done = wait_state(&server, started["id"].as_str().unwrap(), &["complete"]).await;
    let artifact = done["artifact"].as_str().unwrap();
    let mut registration = server.config["models"][0].clone();
    registration["id"] = json!("managed-tamper");
    registration["artifact"] = json!(artifact);
    registration["model_file"] = json!("");
    registration["policy"] = json!("disabled");
    registration["revision"] = json!(0);
    registration["args"][5] = json!(server.directory.path().join("managed-tamper.events"));
    let (status, registered) = server.post("models/register", registration).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let (status,enabled)=server.post("juiced/policy",json!({"model":"managed-tamper","policy":"on-demand","expected_revision":registered["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let path = server
        .config_path
        .parent()
        .unwrap()
        .join("artifacts")
        .join(format!("artifact-{artifact}.bin"));
    let original = std::fs::read(&path).unwrap();
    assert_eq!(original.as_slice(), source.bytes.as_slice());
    let mut changed = original.clone();
    changed[0] ^= 0xff;
    std::fs::write(&path, &changed).unwrap();
    assert_ne!(format!("{:x}", Sha256::digest(&changed)), artifact);
    let config_before = std::fs::read(&server.config_path).unwrap();
    let (status, error) = server.post("load", json!({"model":"managed-tamper"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "model_artifact_changed");
    assert!(!server.event("managed-tamper", "worker-startup", "started"));
    let rejected = server.model("managed-tamper").await;
    assert_eq!(rejected["loaded"], false);
    assert!(rejected["pid"].is_null());
    assert!(rejected["process_lifecycle"].is_null());
    assert!(rejected["runtime_identity"].is_null());
    assert_eq!(rejected["revision"], enabled["revision"]);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), config_before);
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    std::fs::write(&path, &original).unwrap();
    let (status, loaded) = server.post("load", json!({"model":"managed-tamper"})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["revision"], enabled["revision"]);
    assert!(server.event("managed-tamper", "worker-startup", "started"));
    let (status, result) = server
        .post(
            "completions",
            json!({"model":"managed-tamper","prompt":"Recovered immutable artifact pipeline"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["model"], "managed-tamper");
    assert_eq!(
        server
            .post("unload", json!({"model":"managed-tamper"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn duplicate_acquisition_preserves_set_references_file_identity_and_restart_under_every_prefix()
 {
    let source = Source::with_bytes(b"duplicate publication payload".to_vec()).await;
    let mut server = server().await;
    let first = pull(&server, &source, "/v1").await;
    let id = first["id"].as_str().unwrap();
    wait_state(&server, id, &["complete"]).await;
    server
        .wait_for(async || job(&server, id).await["writer_active"] == false)
        .await;
    let original = inventory(&server).await;
    let artifact = original["artifacts"][0]["id"].as_str().unwrap().to_owned();
    let specification = json!({"schema_version":1,"origin":{"importer_id":"rust-process-fixture","importer_version":"1","source_revision":"duplicate-publication"},"primary":"model","members":[{"name":"model","role":"model","artifact":artifact,"bytes":source.bytes.len()}]});
    let (status, record) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":original["generation"],"specification":specification}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{record}");
    let set = record["id"].as_str().unwrap().to_owned();
    let path = server
        .config_path
        .parent()
        .unwrap()
        .join("artifacts")
        .join(format!("artifact-{artifact}.bin"));
    let original_file = std::fs::File::open(&path).unwrap();
    let original_metadata = original_file.metadata().unwrap();
    let configuration = std::fs::read(&server.config_path).unwrap();
    for prefix in PREFIXES {
        let duplicate = pull(&server, &source, prefix).await;
        let id = duplicate["id"].as_str().unwrap();
        let complete = wait_state(&server, id, &["complete"]).await;
        server
            .wait_for(async || job(&server, id).await["writer_active"] == false)
            .await;
        assert_eq!(complete["artifact"], artifact);
        let (status, view) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/artifacts"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["artifacts"].as_array().unwrap().len(), 1);
        assert_eq!(view["artifacts"][0]["active_holds"], 0);
        assert_eq!(
            view["artifacts"][0]["references"],
            json!([format!("artifact-set:{set}")])
        );
        assert_eq!(view["artifact_sets"], json!([record]));
        assert_eq!(
            std::fs::read(&path).unwrap().as_slice(),
            source.bytes.as_slice()
        );
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(
            metadata.modified().unwrap(),
            original_metadata.modified().unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                (metadata.dev(), metadata.ino()),
                (original_metadata.dev(), original_metadata.ino())
            );
        }
        assert_eq!(std::fs::read(&server.config_path).unwrap(), configuration);
    }
    let before_restart = inventory(&server).await;
    server.restart().await;
    assert_eq!(inventory(&server).await, before_restart);
    assert_eq!(
        std::fs::read(&path).unwrap().as_slice(),
        source.bytes.as_slice()
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())),
        artifact
    );
    assert_eq!(source.requests(), 5);
}

async fn query(server: &Server, path: &str) -> (StatusCode, Value) {
    server
        .json(Method::GET, path, None, server.admin_key())
        .await
}

#[tokio::test]
async fn inventory_pages_sections_keeps_off_page_selections_and_refuses_stale_walks_under_every_prefix()
 {
    let server = server().await;
    let mut sources = Vec::new();
    for index in 0..3u8 {
        let source = Source::with_bytes(vec![b'a' + index; 64 + usize::from(index)]).await;
        let started = pull(&server, &source, "/v1").await;
        let id = started["id"].as_str().unwrap().to_owned();
        wait_state(&server, &id, &["complete"]).await;
        server
            .wait_for(async || job(&server, &id).await["writer_active"] == false)
            .await;
        sources.push(source);
    }
    let mut ids: Vec<String> = sources
        .iter()
        .map(|source| format!("{:x}", Sha256::digest(source.bytes.as_slice())))
        .collect();
    ids.sort();
    let member = ids[2].clone();
    let bytes = sources
        .iter()
        .find(|source| format!("{:x}", Sha256::digest(source.bytes.as_slice())) == member)
        .unwrap()
        .bytes
        .len();
    let generation = inventory(&server).await["generation"].clone();
    let (status, set) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":{"schema_version":1,"origin":{"importer_id":"rust-process-fixture","importer_version":"1","source_revision":"paging"},"primary":"model","members":[{"name":"model","role":"model","artifact":member,"bytes":bytes}]}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    let set_id = set["id"].as_str().unwrap().to_owned();
    let configuration = std::fs::read(&server.config_path).unwrap();
    let durable = inventory(&server).await;

    for prefix in PREFIXES {
        let base = format!("{prefix}/juiced/artifacts");
        let (status, first) = query(&server, &format!("{base}?section=artifacts&limit=1")).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["jobs"], json!([]));
        assert_eq!(first["pages"]["jobs"], Value::Null);
        assert_eq!(first["pages"]["artifacts"]["total"], 3);
        let view = first["view"].as_str().unwrap().to_owned();
        let mut walked = vec![first["artifacts"][0]["id"].as_str().unwrap().to_owned()];
        let mut after = first["pages"]["artifacts"]["next_after"].clone();
        while let Some(cursor) = after.as_str() {
            let (status, page) = query(
                &server,
                &format!(
                    "{base}?section=artifacts&limit=1&artifacts_after={cursor}&expected_view={view}"
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["view"], view);
            walked.push(page["artifacts"][0]["id"].as_str().unwrap().to_owned());
            after = page["pages"]["artifacts"]["next_after"].clone();
        }
        assert_eq!(walked, ids);

        let (status, selected) = query(
            &server,
            &format!("{base}?section=jobs&limit=1&artifact={member}&artifact_set={set_id}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        assert_eq!(selected["artifacts"], json!([]));
        assert_eq!(selected["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(selected["pages"]["jobs"]["total"], 3);
        assert_eq!(selected["selection"]["artifact"]["reference_count"], 1);
        assert_eq!(
            selected["selection"]["artifact"]["references"],
            json!([format!("artifact-set:{set_id}")])
        );
        assert_eq!(selected["selection"]["artifact_set"], set);

        let (status, both) = query(
            &server,
            &format!(
                "{base}?limit=1&artifacts_after={}&jobs_after={}&expected_view={view}",
                ids[0],
                selected["jobs"][0]["id"].as_str().unwrap()
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{both}");
        assert_eq!(both["artifacts"][0]["id"], ids[1].as_str());
        assert_eq!(both["jobs"].as_array().unwrap().len(), 1);
        assert_ne!(both["jobs"][0]["id"], selected["jobs"][0]["id"]);
        assert_eq!(both["artifact_sets"][0]["id"], set_id.as_str());

        let job = "00000000000000000000000000000000";
        for invalid in [
            "limit=0".to_owned(),
            "limit=129".to_owned(),
            "after=x".to_owned(),
            format!("jobs_after={job}"),
            format!("section=artifacts&jobs_after={job}&expected_view={view}"),
            format!("artifacts_after={job}&expected_view={view}"),
            "section=everything".to_owned(),
            "unknown=1".to_owned(),
            "expected_view=short".to_owned(),
        ] {
            let (status, body) = query(&server, &format!("{base}?{invalid}")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}: {body}");
        }
        let (status, _) = query(
            &server,
            &format!("{base}?jobs_after={job}&expected_view={view}"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(std::fs::read(&server.config_path).unwrap(), configuration);
        assert_eq!(inventory(&server).await, durable);
    }

    let (status, first) = query(&server, "/v1/juiced/artifacts?section=artifacts&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    let stale_view = first["view"].as_str().unwrap().to_owned();
    let cursor = first["pages"]["artifacts"]["next_after"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, removed) = server
        .post(
            "juiced/artifacts/remove",
            json!({"artifact":ids[0],"expected_generation":durable["generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    for prefix in PREFIXES {
        let (status, body) = query(
            &server,
            &format!(
                "{prefix}/juiced/artifacts?section=artifacts&limit=1&artifacts_after={cursor}&expected_view={stale_view}"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }
    let current = inventory(&server).await;
    assert_eq!(current["pages"]["artifacts"]["total"], 2);
    assert!(
        current["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["id"] != ids[0].as_str())
    );
}
