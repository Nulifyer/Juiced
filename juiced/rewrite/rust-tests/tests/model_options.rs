mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server};
use tokio::io::AsyncReadExt;

async fn server(extra: impl FnOnce(&mut Value)) -> Server {
    Server::configured(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["args"].as_array_mut().unwrap().extend([
            json!("--ctx-size"),
            json!(1024.to_string()),
            json!("--threads"),
            json!("2"),
            json!("--gpu-layers"),
            json!("0"),
            json!("--parallel"),
            json!("1"),
        ]);
        extra(config);
    })
    .await
}
async fn options(server: &Server) -> Value {
    server.get("models/fixture.one/options").await
}
async fn update(server: &Server, expected: &Value, options: Value) -> (StatusCode, Value) {
    server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":expected,"options":options}),
        )
        .await
}

#[tokio::test]
async fn saved_options_and_legacy_effective_launch_round_trip_under_all_prefixes_and_restart() {
    let mut server = server(|_| {}).await;
    let initial = options(&server).await;
    assert_eq!(initial["provider"], "llama-cpp");
    assert_eq!(initial["saved"]["context_tokens"], Value::Null);
    assert_eq!(initial["effective_launch"]["context_tokens"], 1024);
    assert_eq!(initial["effective_launch"]["threads"], 2);
    assert_eq!(initial["physical_slots"], 1);
    for (index, prefix) in PREFIXES.iter().enumerate() {
        let current = options(&server).await;
        let requested = json!({"context_tokens":2048+index,"threads":3+index,"gpu_layers":0,"parallel_slots":1});
        let (status, saved) = server
            .json(
                Method::POST,
                &format!("{prefix}/models/fixture.one/options"),
                Some(json!({"expected_revision":current["revision"],"options":requested})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["saved"], requested);
        assert_eq!(saved["effective_launch"], requested);
        assert_ne!(saved["revision"], current["revision"]);
        let (status, read) = server
            .json(
                Method::GET,
                &format!("{prefix}/models/fixture.one/options"),
                None,
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read, saved);
        assert_eq!(server.model("fixture.one").await["policy"], "on-demand");
        assert_eq!(server.model("fixture.one").await["loaded"], false);
        let (status, error) = update(&server, &current["revision"], json!({"threads":1})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "revision_conflict");
        server.restart().await;
        assert_eq!(options(&server).await, saved);
        assert_eq!(
            server.post("load", json!({"model":"fixture.one"})).await.0,
            StatusCode::OK
        );
        let observed = server.get("juiced/context?model=fixture.one").await;
        assert_eq!(observed["configured"], requested);
        assert_eq!(observed["applied"]["launch"], requested);
        assert_eq!(
            observed["observed"]["props"]["per_request_tokens"],
            requested["context_tokens"]
        );
        assert_eq!(
            observed["observed"]["slots"][0]["context_tokens"],
            requested["context_tokens"]
        );
        assert_eq!(
            server
                .post("unload", json!({"model":"fixture.one"}))
                .await
                .0,
            StatusCode::OK
        );
    }
    let before = options(&server).await;
    let (status, reset) = update(&server, &before["revision"], json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reset["saved"]["threads"], Value::Null);
    assert_eq!(reset["effective_launch"]["threads"], 2);
    assert_eq!(reset["effective_launch"]["context_tokens"], 1024);
    server.restart().await;
    assert_eq!(options(&server).await, reset);
}

#[tokio::test]
async fn invalid_unknown_or_unsafe_options_leave_configuration_and_live_intent_unchanged() {
    let server = server(|_| {}).await;
    let before = options(&server).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    for invalid in [
        json!({"context_tokens":0}),
        json!({"threads":0}),
        json!({"threads":65537}),
        json!({"gpu_layers":-1}),
        json!({"parallel_slots":0}),
        json!({"context_tokens":2147483648u64}),
        json!({"threads":"4"}),
    ] {
        let expected = if invalid["gpu_layers"]
            .as_i64()
            .is_some_and(|value| value < 0)
            || invalid["threads"].is_string()
        {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::BAD_REQUEST
        };
        let (status, error) = update(&server, &before["revision"], invalid).await;
        assert_eq!(status, expected);
        assert!(error["error"]["message"].is_string());
    }
    for field in [
        "command",
        "args",
        "model_file",
        "provider",
        "resident_bytes",
        "max_inflight",
        "policy",
        "essential_headroom",
        "allow_download",
    ] {
        let (status, _) = update(&server, &before["revision"], json!({(field):1})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        let (status, _) = server
            .post(
                "models/fixture.one/options",
                json!({"expected_revision":before["revision"],"options":{},(field):1}),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "top-level {field}"
        );
    }
    let (status, error) = update(&server, &before["revision"], json!({"parallel_slots":2})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "slot_resize_requires_registration");
    assert_eq!(options(&server).await, before);
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(server.model("fixture.one").await["loaded"], false);
}

#[tokio::test]
async fn explicit_provider_and_unloaded_worker_are_required_while_policy_is_preserved() {
    let server = server(|_| {}).await;
    let (status, error) = server
        .post(
            "models/fixture.two/options",
            json!({"expected_revision":0,"options":{"threads":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(error["error"]["code"], "typed_options_unsupported");
    assert_eq!(
        server.get("models/fixture.two/options").await["effective_launch"],
        Value::Null
    );
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    let loaded = server.model("fixture.one").await;
    let before = options(&server).await;
    let (status, error) = update(&server, &before["revision"], json!({"threads":1})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "worker_in_use");
    assert_eq!(options(&server).await, before);
    assert_eq!(server.model("fixture.one").await["pid"], loaded["pid"]);
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    let (status,disabled)=server.post("juiced/policy",json!({"model":"fixture.one","policy":"disabled","expected_revision":before["revision"]})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        update(&server, &disabled["revision"], json!({"threads":1}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(server.model("fixture.one").await["policy"], "disabled");
    assert_eq!(server.model("fixture.one").await["loaded"], false);
}

#[tokio::test]
async fn failed_options_publication_retains_original_revision_and_profile() {
    let mut server = server(|_| {}).await;
    let before = options(&server).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    std::fs::remove_file(&server.config_path).unwrap();
    std::fs::create_dir(&server.config_path).unwrap();
    let (status, error) = update(&server, &before["revision"], json!({"threads":7})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"]["code"], "policy_save");
    assert_eq!(options(&server).await, before);
    std::fs::remove_dir(&server.config_path).unwrap();
    std::fs::write(&server.config_path, &disk).unwrap();
    server.restart().await;
    assert_eq!(options(&server).await, before);
}

#[tokio::test]
async fn changing_an_unloaded_profile_rejects_queued_old_worker_before_dispatch() {
    let server = server(|config| config["admission"] = json!({"active_limit":1})).await;
    let held = server
        .socket_request(
            "completions",
            json!({"model":"fixture.two","test_id":"hold-other","fixture_delay_ms":2000}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.two", "hold-other", "started"))
        .await;
    let mut queued = server
        .socket_request(
            "completions",
            json!({"model":"fixture.one","test_id":"old-options"}),
        )
        .await;
    server
        .wait_for(async || server.get("juiced/admission").await["scheduler"]["pending"] == 1)
        .await;
    let before = options(&server).await;
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    assert_eq!(
        update(&server, &before["revision"], json!({"threads":3}))
            .await
            .0,
        StatusCode::OK
    );
    drop(held);
    let mut bytes = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let mut part = [0; 1024];
            let count = queued.read(&mut part).await.unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&part[..count]);
            if bytes
                .windows(b"model_replaced".len())
                .any(|part| part == b"model_replaced")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let response = String::from_utf8(bytes).unwrap();
    assert!(response.starts_with("HTTP/1.1 409"));
    assert!(response.contains("model_replaced"));
    assert!(!server.event("fixture.one", "old-options", "started"));
    server
        .wait_for(async || {
            server.get("juiced/admission").await["scheduler"]["active"] == 0
                && server.get("juiced/admission").await["scheduler"]["pending"] == 0
        })
        .await;
    let (status, result) = server
        .post("completions", json!({"model":"fixture.one"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["model"], "fixture.one");
}
