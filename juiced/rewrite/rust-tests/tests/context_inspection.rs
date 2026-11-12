mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server, metadata_gguf};

async fn context(server: &Server, model: &str) -> Value {
    server.get(&format!("juiced/context?model={model}")).await
}

#[tokio::test]
async fn unloaded_context_inspection_is_read_only_and_unknown_metadata_stays_unknown() {
    let server = Server::new(true).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    let before = server.get("juiced/state").await;
    let records = server.get("juiced/requests").await;
    for prefix in PREFIXES {
        let (status, view) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/context?model=fixture.one"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["schema_version"], 1);
        assert_eq!(view["qualification"], "unqualified");
        for field in ["declared", "configured", "applied", "observed"] {
            assert_eq!(view[field], Value::Null, "{field}");
        }
        assert_eq!(view["observation_error"], "worker_unloaded");
        assert_eq!(view["provider"], "external-http");
    }
    assert_eq!(server.get("juiced/state").await, before);
    assert_eq!(server.get("juiced/requests").await, records);
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    for (query, status) in [
        ("", StatusCode::BAD_REQUEST),
        ("?model=", StatusCode::BAD_REQUEST),
        ("?model=missing", StatusCode::NOT_FOUND),
        ("?model=fixture.one&unknown=1", StatusCode::BAD_REQUEST),
    ] {
        let (actual, error) = server
            .json(
                Method::GET,
                &format!("/v1/juiced/context{query}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(actual, status);
        assert!(error["error"]["message"].is_string());
    }
}

#[tokio::test]
async fn declared_launch_applied_and_engine_observed_context_are_distinct_facts() {
    let mut server = Server::configured(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(2);
        model["options"] =
            json!({"context_tokens":2048,"threads":3,"gpu_layers":0,"parallel_slots":2});
        std::fs::write(model["model_file"].as_str().unwrap(), metadata_gguf(8192)).unwrap();
    })
    .await;
    let unloaded = context(&server, "fixture.one").await;
    assert_eq!(unloaded["declared"]["format_version"], 3);
    assert_eq!(unloaded["declared"]["architecture"], "llama");
    assert_eq!(unloaded["declared"]["training_context_tokens"], 8192);
    assert_eq!(unloaded["configured"]["context_tokens"], 2048);
    assert_eq!(unloaded["applied"], Value::Null);
    assert_eq!(unloaded["observed"], Value::Null);
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    let loaded = context(&server, "fixture.one").await;
    assert_eq!(loaded["declared"], unloaded["declared"]);
    assert_eq!(loaded["configured"], unloaded["configured"]);
    assert_eq!(loaded["applied"]["source"], "spawned argument vector");
    assert_eq!(loaded["applied"]["launch"], loaded["configured"]);
    assert_eq!(loaded["applied"]["engine_confirmed"], false);
    assert_eq!(
        loaded["applied"]["pid"],
        server.model("fixture.one").await["pid"]
    );
    assert_eq!(
        loaded["applied"]["execution_fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(loaded["observation_error"], Value::Null);
    assert_eq!(loaded["observed"]["props"]["slots"], 2);
    assert_eq!(loaded["observed"]["props"]["per_request_tokens"], 2048);
    let slots = loaded["observed"]["slots"].as_array().unwrap();
    assert_eq!(slots.len(), 2);
    for slot in slots {
        assert_eq!(slot["context_tokens"], 2048);
        assert_eq!(slot["is_processing"], false);
    }
    assert_eq!(server.get("juiced/requests").await["data"], json!([]));
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    server.restart().await;
    let restarted = context(&server, "fixture.one").await;
    assert_eq!(restarted["declared"], unloaded["declared"]);
    assert_eq!(restarted["configured"], unloaded["configured"]);
    assert_eq!(restarted["applied"], Value::Null);
    assert_eq!(restarted["observed"], Value::Null);
}

#[tokio::test]
async fn selected_llama_provider_rejects_mismatched_actual_slots_and_releases_reservation() {
    let server = Server::configured(true, |config| {
        config["models"][0]["provider"] = json!("llama-cpp");
        config["models"][0]["max_inflight"] = json!(1);
    })
    .await;
    let (status, error) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error["error"]["code"], "worker_slots_mismatch");
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    assert_eq!(server.model("fixture.one").await["pid"], Value::Null);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    let before = server.get("models/fixture.one/options").await;
    let (status, _) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":before["revision"],"options":{"parallel_slots":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        context(&server, "fixture.one").await["observed"]["slots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn external_provider_reports_applied_process_without_claiming_native_observation() {
    let server = Server::new(true).await;
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    let view = context(&server, "fixture.one").await;
    assert_eq!(view["declared"], Value::Null);
    assert_eq!(view["configured"], Value::Null);
    assert_eq!(view["applied"]["launch"], Value::Null);
    assert_eq!(view["applied"]["engine_confirmed"], false);
    assert_eq!(view["observed"], Value::Null);
    assert_eq!(
        view["observation_error"],
        "provider_observation_unsupported"
    );
}
