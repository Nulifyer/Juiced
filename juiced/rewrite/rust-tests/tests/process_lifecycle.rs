#![cfg(target_os = "linux")]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::Server;

fn startup_event(server: &Server) -> Value {
    std::fs::read_to_string(server.directory.path().join("fixture.one.events"))
        .unwrap()
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["id"] == "worker-startup" && event["event"] == "started")
        .expect("complete worker-startup event")
}
fn gone(pid: u32) -> bool {
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[tokio::test]
async fn cancelled_startup_reconciles_owned_child_and_reservation_before_retrying_a_new_generation()
{
    let unrelated = Server::new(true).await;
    assert!(
        unrelated
            .post("load", json!({"model":"fixture.one"}))
            .await
            .0
            .is_success()
    );
    let unrelated_pid = unrelated.model("fixture.one").await["pid"].clone();
    let server = Server::configured(true, |config| {
        config["resident_budget_bytes"] = json!(64);
        config["admission"] = json!({"startup":{"concurrent":2}});
        config["startup_timeout_ms"] = json!(5000);
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--startup-delay-ms"), json!("750")]);
    })
    .await;
    let request = server
        .socket_request(
            "completions",
            json!({"model":"fixture.one","test_id":"startup-cancel"}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let event = startup_event(&server);
    let pid = event["pid"].as_u64().unwrap() as u32;
    assert!(!gone(pid));
    let starting = server.model("fixture.one").await;
    let generation = starting["process_lifecycle"]["generation"]
        .as_u64()
        .unwrap();
    assert_eq!(starting["process_lifecycle"]["pid"], pid);
    assert_eq!(starting["state"], "loading");
    assert_eq!(starting["process_lifecycle"]["phase"], "running");
    assert_eq!(starting["preparation_pending"], true);
    assert_eq!(
        server.get("juiced/admission").await["pending_preparations"],
        json!(["fixture.one"])
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        64
    );
    assert_eq!(
        server.post("load", json!({"model":"fixture.two"})).await.1["error"]["code"],
        "resident_budget"
    );
    drop(request);
    server
        .wait_for(async || {
            let model = server.model("fixture.one").await;
            model["process_lifecycle"].is_null()
                && model["preparation_pending"] == false
                && server.get("juiced/admission").await["pending_preparations"]
                    .as_array()
                    .unwrap()
                    .is_empty()
                && model["loaded"] == false
                && server.get("juiced/state").await["resources"]["reserved_bytes"] == 0
        })
        .await;
    assert!(
        gone(pid),
        "startup resources were released before the owned child exit was confirmed"
    );
    assert_eq!(server.model("fixture.one").await["pid"], Value::Null);
    let (status, retried) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(retried["process_lifecycle"]["phase"], "running");
    assert!(retried["process_lifecycle"]["generation"].as_u64().unwrap() > generation);
    assert_ne!(retried["pid"], pid);
    let retried_pid = retried["pid"].as_u64().unwrap() as u32;
    let (status, unloaded) = server.post("unload", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unloaded["loaded"], false);
    assert_eq!(unloaded["pid"], Value::Null);
    assert_eq!(unloaded["process_lifecycle"], Value::Null);
    assert_eq!(unloaded["preparation_pending"], false);
    assert!(
        server.get("juiced/admission").await["pending_preparations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        gone(retried_pid),
        "unload returned before confirming the owned worker exit"
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(unrelated.model("fixture.one").await["pid"], unrelated_pid);
    let (status, result) = unrelated
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Unrelated process remains usable"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["model"], "fixture.one");
}
