#![cfg(feature = "test-fixture")]

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use juiced_service::{
    config::{Config, ModelSpec, Operation, Policy},
    http::{Auth, router},
    supervisor::Manager,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

struct Harness {
    dir: tempfile::TempDir,
    path: PathBuf,
    config: Config,
    manager: Arc<Manager>,
}

impl Harness {
    fn new() -> Self {
        Self::configured(|_| {})
    }

    fn configured(change: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let weights = dir.path().join("fixture.gguf");
        std::fs::write(&weights, "fixture bytes, not real weights").unwrap();
        let model = |id: &str| ModelSpec {
            resource_profile: None,
            id: id.into(),
            model_file: weights.clone(),
            artifact: None,
            artifact_set: None,
            previous_artifact_set: None,
            execution_profile: juiced_service::execution_profile::ExecutionProfile::from_legacy(
                Default::default(),
                None,
                Default::default(),
                env!("CARGO_BIN_EXE_juiced-test-worker").into(),
                vec![
                    "--port".into(),
                    "{port}".into(),
                    "--model".into(),
                    "{model}".into(),
                    "--events".into(),
                    dir.path()
                        .join(format!("{id}.events"))
                        .to_string_lossy()
                        .into(),
                ],
            ),
            resident_bytes: 64,
            max_inflight: 2,
            essential_headroom: 0,
            operations: vec![Operation::Chat, Operation::Completion, Operation::Responses],
            policy: Policy::OnDemand,
            revision: 0,
            idle_unload_after_ms: None,
            companions: Vec::new(),
        };
        let mut config = Config {
            schema_version: 2,
            native_settings: None,
            generation: 0,
            cache_enabled: false,
            session_cache: Default::default(),
            inhibit_suspend: false,
            admission: Default::default(),
            artifact_source_policy: juiced_service::artifacts::SourcePolicy::PublicHttps,
            resident_budget_bytes: 128,
            idle_unload_after_ms: juiced_service::config::DEFAULT_IDLE_UNLOAD_MS,
            startup_timeout_ms: 2000,
            request_timeout_ms: 3000,
            models: vec![model("one"), model("two")],
            aliases: Default::default(),
            upload_max_bytes: juiced_service::config::DEFAULT_UPLOAD_MAX_BYTES,
            import_roots: Vec::new(),
            wyoming: Default::default(),
            protocols: Default::default(),
            barista: Default::default(),
            native_adoption: None,
        };
        change(&mut config);
        let path = dir.path().join("config.json");
        config.persist(&path).unwrap();
        let manager = Manager::new(config.clone(), path.clone()).unwrap();
        Self {
            dir,
            path,
            config,
            manager,
        }
    }

    fn app(&self) -> axum::Router {
        router(self.manager.clone(), Auth::default(), None)
    }

    async fn serve(&self) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = self.app();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, task)
    }

    async fn call(&self, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        call(self.app(), path, body, None).await
    }

    fn has_event(&self, model: &str, id: &str, event: &str) -> bool {
        std::fs::read_to_string(self.dir.path().join(format!("{model}.events")))
            .unwrap_or_default()
            .lines()
            .any(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                value["id"] == id && value["event"] == event
            })
    }
}

async fn call(
    app: axum::Router,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request =
        Request::builder()
            .uri(path)
            .method(if body.is_some() { "POST" } else { "GET" });
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .header("content-type", "application/json")
        .body(Body::from(
            body.map(|body| body.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn eventually(mut condition: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition did not become true within two seconds");
}

#[test]
fn configuration_rejects_duplicate_ids_missing_slots_and_invalid_budget() {
    let h = Harness::new();
    for change in [0, 1, 2, 3] {
        let mut config = h.config.clone();
        match change {
            0 => config.models[1].id = "one".into(),
            1 => config.models[0].max_inflight = 0,
            2 => config.resident_budget_bytes = 1,
            _ => config.models[0].execution_profile.args.clear(),
        }
        assert!(config.validate().is_err());
    }
    let mut value = serde_json::to_value(&h.config.models[0]).unwrap();
    value.as_object_mut().unwrap().remove("policy");
    assert_eq!(
        serde_json::from_value::<ModelSpec>(value).unwrap().policy,
        Policy::Disabled
    );
}

#[tokio::test]
async fn all_prefixes_enforce_regular_and_operator_authority() {
    let h = Harness::new();
    let app = router(
        h.manager.clone(),
        Auth {
            api_key: Some("client".into()),
            admin_key: Some("operator".into()),
        },
        None,
    );
    for prefix in ["/api/v0", "/api/v1", "/v0", "/v1"] {
        for endpoint in [
            "health",
            "models",
            "juiced/catalog",
            "juiced/state",
            "juiced/protocols",
            "juiced/suitability?model=one",
            "juiced/requests",
            "unknown",
        ] {
            assert_eq!(
                call(app.clone(), &format!("{prefix}/{endpoint}"), None, None)
                    .await
                    .0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                call(
                    app.clone(),
                    &format!("{prefix}/{endpoint}"),
                    None,
                    Some("bad")
                )
                .await
                .0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                call(
                    app.clone(),
                    &format!("{prefix}/{endpoint}"),
                    None,
                    Some("client")
                )
                .await
                .0,
                if endpoint.starts_with("juiced/suitability") || endpoint == "juiced/requests" {
                    StatusCode::UNAUTHORIZED
                } else if endpoint == "unknown" {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::OK
                }
            );
            assert_eq!(
                call(
                    app.clone(),
                    &format!("{prefix}/{endpoint}"),
                    None,
                    Some("operator")
                )
                .await
                .0,
                if endpoint == "unknown" {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::OK
                }
            );
        }
        for endpoint in ["load", "unload", "juiced/policy"] {
            assert_eq!(
                call(
                    app.clone(),
                    &format!("{prefix}/{endpoint}"),
                    Some(json!({"model": "one"})),
                    Some("client")
                )
                .await
                .0,
                StatusCode::UNAUTHORIZED
            );
        }
        for endpoint in ["chat/completions", "completions", "responses"] {
            assert_eq!(
                call(
                    app.clone(),
                    &format!("{prefix}/{endpoint}"),
                    Some(json!({})),
                    None
                )
                .await
                .0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                call(
                    app.clone(),
                    &format!("{prefix}/{endpoint}"),
                    Some(json!({})),
                    Some("client")
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            call(
                app.clone(),
                &format!("{prefix}/juiced/policy"),
                Some(json!({"model":"one", "policy":"disabled", "expected_revision":0})),
                Some("operator")
            )
            .await
            .0,
            if prefix == "/api/v0" {
                StatusCode::OK
            } else {
                StatusCode::CONFLICT
            }
        );
    }
    let app = router(
        h.manager.clone(),
        Auth {
            api_key: None,
            admin_key: Some("operator".into()),
        },
        None,
    );
    assert_eq!(
        call(app.clone(), "/v1/health", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(app, "/v1/health", None, Some("operator")).await.0,
        StatusCode::OK
    );
}

// A process forked by a concurrent test holds the inherited lock descriptor until exec.
async fn reopen(path: PathBuf) -> Arc<Manager> {
    for _ in 0..200 {
        match Manager::open(path.clone()) {
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock) =>
            {
                tokio::time::sleep(Duration::from_millis(5)).await
            }
            other => return other.unwrap(),
        }
    }
    Manager::open(path).unwrap()
}

#[tokio::test]
async fn policy_is_durable_and_concurrent_edits_have_one_winner() {
    let h = Harness::new();
    let (left, right) = tokio::join!(
        h.manager.set_policy("one", Policy::Hot, 0),
        h.manager.set_policy("one", Policy::Disabled, 0)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let winning = left.or(right).unwrap();
    h.manager.shutdown().await;
    let Harness {
        dir: _directory,
        path,
        manager,
        ..
    } = h;
    drop(manager);
    let restarted = reopen(path).await;
    let view = restarted.catalog().await;
    assert_eq!(view[0].revision, 1);
    assert_eq!(view[0].policy, winning.policy);
    assert!(!view[0].loaded);
    restarted.shutdown().await;
}

#[tokio::test]
async fn persistence_failure_keeps_the_published_policy_unchanged() {
    let h = Harness::new();
    std::fs::remove_file(&h.path).unwrap();
    std::fs::create_dir(&h.path).unwrap();
    assert_eq!(
        h.manager
            .set_policy("one", Policy::Hot, 0)
            .await
            .unwrap_err()
            .code,
        "policy_save"
    );
    let view = h.manager.catalog().await;
    assert_eq!(view[0].policy, Policy::OnDemand);
    assert_eq!(view[0].revision, 0);
}

#[tokio::test]
async fn disabled_models_are_hidden_and_cannot_load_or_run() {
    let h = Harness::configured(|config| config.models[0].policy = Policy::Disabled);
    assert_eq!(
        h.manager.load("one").await.unwrap_err().code,
        "model_disabled"
    );
    let (status, _) = h
        .call("/v1/chat/completions", Some(json!({"model":"one"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, models) = h.call("/v1/models", None).await;
    assert_eq!(models["data"].as_array().unwrap().len(), 1);
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
}

#[tokio::test]
async fn repeated_load_uses_one_process_and_hot_blocks_unload() {
    let h = Harness::new();
    let (left, right) = tokio::join!(h.manager.load("one"), h.manager.load("one"));
    assert_eq!(left.unwrap().pid, right.unwrap().pid);
    assert_eq!(h.manager.resources()["reserved_bytes"], 64);
    h.manager.set_policy("one", Policy::Hot, 0).await.unwrap();
    assert_eq!(
        h.manager.unload("one").await.unwrap_err().code,
        "worker_hot"
    );
    assert_eq!(
        h.manager
            .set_policy("one", Policy::Disabled, 1)
            .await
            .unwrap_err()
            .code,
        "worker_loaded"
    );
    h.manager
        .set_policy("one", Policy::OnDemand, 1)
        .await
        .unwrap();
    h.manager.unload("one").await.unwrap();
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
}

#[tokio::test]
async fn resident_and_slot_pressure_release_without_evicting_other_workers() {
    let h = Harness::configured(|config| config.resident_budget_bytes = 64);
    let (_, first) = h.manager.begin("one", Operation::Chat).await.unwrap();
    let (_, second) = h.manager.begin("one", Operation::Chat).await.unwrap();
    assert!(
        matches!(h.manager.begin("one", Operation::Chat).await, Err(error) if error.code == "request_slots")
    );
    assert!(
        matches!(h.manager.begin("two", Operation::Chat).await, Err(error) if error.code == "resident_budget")
    );
    assert_eq!(
        h.manager.unload("one").await.unwrap_err().code,
        "worker_busy"
    );
    drop(first);
    drop(second);
    h.manager.unload("one").await.unwrap();
    assert!(h.manager.load("two").await.unwrap().loaded);
    h.manager.shutdown().await;
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
}

#[tokio::test]
async fn startup_failure_and_timeout_release_reservations_for_retry() {
    for mode in ["exit", "hang"] {
        let h = Harness::configured(|config| {
            config.startup_timeout_ms = 1500;
            config.models[0]
                .execution_profile
                .args
                .extend(["--mode".into(), mode.into()]);
        });
        assert_eq!(
            h.manager.load("one").await.unwrap_err().code,
            "worker_not_ready"
        );
        assert_eq!(h.manager.resources()["reserved_bytes"], 0);
        assert!(h.manager.load("two").await.unwrap().loaded);
        h.manager.shutdown().await;
    }
}

#[tokio::test]
async fn slow_startup_keeps_catalog_and_other_workers_responsive() {
    let h = Harness::configured(|config| {
        config.admission.startup.concurrent = 2;
        config.startup_timeout_ms = 30_000;
        config.models[0]
            .execution_profile
            .args
            .extend(["--startup-delay-ms".into(), "5000".into()])
    });
    let manager = h.manager.clone();
    let loading = tokio::spawn(async move { manager.load("one").await });
    eventually(async || h.manager.resources()["reserved_bytes"] == 64).await;
    let view = tokio::time::timeout(Duration::from_millis(1000), h.manager.catalog())
        .await
        .expect("catalog blocked on startup");
    assert_eq!(view[0].state, "loading");
    let other = tokio::time::timeout(Duration::from_millis(4000), h.manager.load("two"))
        .await
        .expect("unrelated load blocked")
        .unwrap();
    assert!(other.loaded);
    assert!(
        !loading.is_finished(),
        "the slow startup finished before the unrelated load"
    );
    assert_eq!(h.manager.catalog().await[0].state, "loading");
    assert!(loading.await.unwrap().unwrap().loaded);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn ready_worker_reuses_its_generation_while_another_startup_owns_the_only_permit() {
    let mut release = None;
    let h = Harness::configured(|config| {
        let path = config.models[0].model_file.with_extension("release");
        config.models[0].execution_profile.args.extend([
            "--startup-release-file".into(),
            path.to_string_lossy().into_owned(),
        ]);
        release = Some(path);
    });
    let ready = h.manager.load("two").await.unwrap();
    let manager = h.manager.clone();
    let loading = tokio::spawn(async move { manager.load("one").await });
    eventually(async || h.manager.resources()["reserved_bytes"] == 128).await;
    let catalog = tokio::time::timeout(Duration::from_millis(100), h.manager.catalog())
        .await
        .expect("catalog remains available during owned startup");
    assert_eq!(catalog[0].state, "loading");
    let reused = tokio::time::timeout(Duration::from_millis(250), h.manager.load("two"))
        .await
        .expect("ready worker must bypass the occupied startup gate")
        .unwrap();
    assert_eq!(reused.pid, ready.pid);
    assert_eq!(
        reused.process_lifecycle.unwrap().generation,
        ready.process_lifecycle.unwrap().generation
    );
    assert_eq!(h.manager.admission()["startup"]["active"], 1);
    assert_eq!(h.manager.admission()["startup"]["pending"], 0);
    std::fs::write(release.unwrap(), b"release").unwrap();
    assert!(loading.await.unwrap().unwrap().loaded);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn cancelled_startup_releases_its_budget_and_allows_other_workers() {
    let h = Harness::configured(|config| {
        config.models[0]
            .execution_profile
            .args
            .extend(["--mode".into(), "hang".into()])
    });
    let manager = h.manager.clone();
    let loading = tokio::spawn(async move { manager.load("one").await });
    eventually(async || h.manager.catalog().await[0].state == "loading").await;
    loading.abort();
    assert!(loading.await.unwrap_err().is_cancelled());
    eventually(async || h.manager.resources()["reserved_bytes"] == 0).await;
    assert_eq!(h.manager.catalog().await[0].state, "unloaded");
    assert!(h.manager.load("two").await.unwrap().loaded);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn hot_startup_reports_failure_without_loading_disabled_or_on_demand_models() {
    let h = Harness::configured(|config| {
        config.models[0].policy = Policy::Hot;
        config.models[0].model_file.set_file_name("missing.gguf");
    });
    h.manager.start_hot().await;
    let view = h.manager.catalog().await;
    assert_eq!(view[0].error, Some("missing_weights"));
    assert!(!view[1].loaded);
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
}

#[tokio::test]
async fn unsupported_operations_do_not_launch_workers() {
    let h = Harness::configured(|config| config.models[0].operations = vec![Operation::Chat]);
    let (status, response) = h.call("/v1/responses", Some(json!({"model":"one"}))).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(response["error"]["code"], "unsupported_operation");
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
    assert!(!h.manager.catalog().await[0].loaded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_loads_cannot_overcommit_the_resident_budget() {
    let h = Harness::configured(|config| config.resident_budget_bytes = 64);
    let (one, two) = tokio::join!(h.manager.load("one"), h.manager.load("two"));
    for result in [&one, &two] {
        if let Err(error) = result {
            assert_eq!(error.code, "resident_budget");
        }
    }
    assert!(one.is_ok() || two.is_ok());
    assert_eq!(h.manager.resources()["reserved_bytes"], 64);
    assert_eq!(
        h.manager
            .catalog()
            .await
            .iter()
            .filter(|model| model.loaded)
            .count(),
        1
    );
    h.manager.shutdown().await;
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
}

#[tokio::test]
async fn shutdown_interrupts_readiness_and_rejects_new_loads() {
    let h = Harness::configured(|config| {
        config.startup_timeout_ms = 30000;
        config.models[0]
            .execution_profile
            .args
            .extend(["--mode".into(), "hang".into()]);
    });
    let manager = h.manager.clone();
    let loading = tokio::spawn(async move { manager.load("one").await });
    eventually(async || h.manager.catalog().await[0].state == "loading").await;
    tokio::time::timeout(Duration::from_millis(500), h.manager.shutdown())
        .await
        .expect("shutdown waited for the startup deadline");
    assert!(loading.await.unwrap().is_err());
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
    assert_eq!(
        h.manager.load("two").await.unwrap_err().code,
        "shutting_down"
    );
}

#[tokio::test]
async fn openai_operations_forward_body_and_preserve_status_without_forwarding_credentials() {
    let h = Harness::new();
    let app = router(
        h.manager.clone(),
        Auth {
            api_key: Some("private-key".into()),
            admin_key: None,
        },
        None,
    );
    for operation in ["chat/completions", "completions", "responses"] {
        let (status, response) = call(app.clone(), &format!("/v1/{operation}"), Some(json!({"model":"one", "messages":[{"role":"user", "content":"secret prompt never in diagnostics"}]})), Some("private-key")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["model"], "one");
        assert_eq!(response["received_authorization"], false);
    }
    let (status, response) = call(
        app,
        "/v1/chat/completions",
        Some(json!({"model":"one", "fixture_status":429})),
        Some("private-key"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response["error"]["message"], "fixture failure");
    let (_, records) = h.call("/v1/juiced/requests", None).await;
    assert!(!records.to_string().contains("secret prompt"));
    assert!(!records.to_string().contains("private-key"));
    assert_eq!(records["data"].as_array().unwrap().len(), 4);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn upstream_timeout_releases_request_slot_and_preserves_worker() {
    let h = Harness::configured(|config| config.request_timeout_ms = 80);
    let before = h.manager.load("one").await.unwrap().pid;
    let (status, _) = h
        .call(
            "/v1/chat/completions",
            Some(json!({"model":"one", "fixture_delay_ms":500})),
        )
        .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(h.manager.resources()["active_requests"], 0);
    assert_eq!(h.manager.load("one").await.unwrap().pid, before);
    assert_eq!(h.manager.records()[0].outcome, "timeout");
    h.manager.shutdown().await;
}

#[tokio::test]
async fn worker_crash_is_scoped_and_next_request_can_restart_it() {
    let h = Harness::new();
    let before = h.manager.load("one").await.unwrap().pid;
    let unrelated = h.manager.load("two").await.unwrap().pid;
    assert_eq!(
        h.call(
            "/v1/chat/completions",
            Some(json!({"model":"one", "fixture_crash":true}))
        )
        .await
        .0,
        StatusCode::BAD_GATEWAY
    );
    eventually(async || h.manager.catalog().await[0].error == Some("worker_exited")).await;
    assert_ne!(h.manager.load("one").await.unwrap().pid, before);
    assert_eq!(h.manager.load("two").await.unwrap().pid, unrelated);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn request_history_is_bounded_and_records_only_metadata() {
    let h = Harness::new();
    for _ in 0..140 {
        let (_, lease) = h.manager.begin("one", Operation::Chat).await.unwrap();
        drop(lease);
    }
    assert_eq!(h.manager.records().len(), 128);
    assert_eq!(h.manager.records()[0].id, 140);
    assert_eq!(h.manager.resources()["active_requests"], 0);
    h.manager.shutdown().await;
}

#[tokio::test]
async fn suitability_is_read_only_under_every_prefix_and_preserves_unknown_fit() {
    let h = Harness::configured(|config| config.models[0].policy = Policy::Disabled);
    let before_file = std::fs::read(&h.path).unwrap();
    let before_resources = h.manager.resources();
    for prefix in ["/api/v0", "/api/v1", "/v0", "/v1"] {
        let (status, assessment) = h
            .call(&format!("{prefix}/juiced/suitability?model=one"), None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(assessment["read_only"], true);
        assert_eq!(assessment["reservation"], false);
        assert_eq!(assessment["enabled"], false);
        assert_eq!(assessment["policy_revision"], 0);
        assert_eq!(assessment["context_tokens"], Value::Null);
        assert_eq!(assessment["compatibility"]["compatible"], Value::Null);
        assert_eq!(assessment["resources"]["fit"], "unknown");
        assert_eq!(assessment["resources"]["hardware_qualified"], false);
        assert_eq!(assessment["resources"]["recommendation_qualified"], false);
        assert_eq!(
            assessment["resources"]["measured_memory_bytes"],
            Value::Null
        );
        assert_eq!(
            assessment["resources"]["declared_budget"]["fits_additional_reservation"],
            true
        );
        assert!(
            assessment["resources"]["blockers"]
                .as_array()
                .unwrap()
                .contains(&json!("model_disabled"))
        );
    }
    assert_eq!(std::fs::read(&h.path).unwrap(), before_file);
    assert_eq!(h.manager.resources(), before_resources);
    assert!(h.manager.records().is_empty());
    assert!(h.manager.catalog().await.iter().all(|model| !model.loaded));
}

#[tokio::test]
async fn suitability_reports_pressure_without_eviction_or_qualification() {
    let h = Harness::configured(|config| config.resident_budget_bytes = 64);
    let (_, lease) = h.manager.begin("one", Operation::Chat).await.unwrap();
    let before = h.manager.resources();
    let pid = h.manager.catalog().await[0].pid;
    let assessment = h.manager.suitability("two").unwrap();
    assert_eq!(assessment["resources"]["fit"], "unknown");
    assert_eq!(
        assessment["resources"]["declared_budget"]["fits_additional_reservation"],
        false
    );
    assert_eq!(
        assessment["resources"]["declared_budget"]["available_bytes"],
        0
    );
    assert!(
        assessment["resources"]["blockers"]
            .as_array()
            .unwrap()
            .contains(&json!("resident_budget"))
    );
    assert_eq!(h.manager.resources(), before);
    assert_eq!(h.manager.catalog().await[0].pid, pid);
    assert!(h.manager.records().is_empty());
    drop(lease);
    h.manager.unload("one").await.unwrap();
    assert_eq!(
        h.manager.suitability("two").unwrap()["resources"]["declared_budget"]["fits_additional_reservation"],
        true
    );
}

#[tokio::test]
async fn suitability_does_not_wait_for_startup_and_keeps_blockers_separate() {
    let h = Harness::configured(|config| {
        config.models[0]
            .execution_profile
            .args
            .extend(["--startup-delay-ms".into(), "500".into()])
    });
    let manager = h.manager.clone();
    let loading = tokio::spawn(async move { manager.load("one").await });
    eventually(async || h.manager.catalog().await[0].state == "loading").await;
    let (status, assessment) = tokio::time::timeout(
        Duration::from_millis(100),
        h.call("/v1/juiced/suitability?model=one", None),
    )
    .await
    .expect("inspection blocked on lifecycle I/O");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(assessment["resources"]["fit"], "unknown");
    assert!(
        assessment["resources"]["blockers"]
            .as_array()
            .unwrap()
            .contains(&json!("worker_transition"))
    );
    loading.await.unwrap().unwrap();
    h.manager.shutdown().await;
    let assessment = h.manager.suitability("one").unwrap();
    assert_eq!(assessment["resources"]["admission_open"], false);
    assert_eq!(assessment["resources"]["fit"], "unknown");
}

#[tokio::test]
async fn suitability_rejects_missing_or_unknown_models_and_reports_missing_weights() {
    let h = Harness::configured(|config| config.models[0].model_file.set_file_name("missing.gguf"));
    for path in [
        "/v1/juiced/suitability",
        "/v1/juiced/suitability?model=",
        "/v1/juiced/suitability?model=not-registered",
    ] {
        assert_eq!(h.call(path, None).await.0, StatusCode::BAD_REQUEST);
    }
    let assessment = h.manager.suitability("one").unwrap();
    assert_eq!(assessment["downloaded"], false);
    assert!(
        assessment["resources"]["blockers"]
            .as_array()
            .unwrap()
            .contains(&json!("missing_weights"))
    );
    assert_eq!(h.manager.resources()["reserved_bytes"], 0);
}

#[tokio::test]
async fn disconnect_during_non_streaming_prefill_aborts_upstream_without_reload() {
    let h = Harness::configured(|config| config.request_timeout_ms = 10000);
    let pid = h.manager.load("one").await.unwrap().pid;
    let unrelated = h.manager.load("two").await.unwrap().pid;
    let (base, server) = h.serve().await;
    let body = json!({"model":"one", "test_id":"prefill", "fixture_delay_ms":5000}).to_string();
    let mut socket = tokio::net::TcpStream::connect(base.trim_start_matches("http://"))
        .await
        .unwrap();
    socket.write_all(format!("POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    eventually(async || h.has_event("one", "prefill", "started")).await;
    drop(socket);
    eventually(async || h.manager.resources()["active_requests"] == 0).await;
    eventually(async || h.has_event("one", "prefill", "cancelled")).await;
    assert_eq!(h.manager.load("one").await.unwrap().pid, pid);
    assert_eq!(h.manager.load("two").await.unwrap().pid, unrelated);
    assert_eq!(h.manager.records()[0].outcome, "cancelled");
    server.abort();
    h.manager.shutdown().await;
}

#[tokio::test]
async fn disconnect_during_streaming_drops_upstream_and_releases_busy_guard() {
    let h = Harness::new();
    let (base, server) = h.serve().await;
    let body = json!({"model":"one", "test_id":"stream", "stream":true}).to_string();
    let mut socket = tokio::net::TcpStream::connect(base.trim_start_matches("http://"))
        .await
        .unwrap();
    socket.write_all(format!("POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut bytes = [0u8; 2048];
    assert!(socket.read(&mut bytes).await.unwrap() > 0);
    eventually(async || h.manager.resources()["active_requests"] == 1).await;
    drop(socket);
    eventually(async || h.manager.resources()["active_requests"] == 0).await;
    eventually(async || h.has_event("one", "stream", "cancelled")).await;
    assert!(!h.manager.unload("one").await.unwrap().loaded);
    server.abort();
    h.manager.shutdown().await;
}
