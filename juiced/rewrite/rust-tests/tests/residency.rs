#![cfg(target_os = "linux")]

mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use support::Server;

fn idle(milliseconds: u64) -> impl FnOnce(&mut Value) {
    move |config| config["idle_unload_after_ms"] = json!(milliseconds)
}

fn add_model(config: &mut Value, id: &str) {
    let mut model = config["models"][0].clone();
    model["id"] = json!(id);
    let events = model["args"].as_array_mut().unwrap();
    let path = events
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .replace("fixture.one", id);
    *events.last_mut().unwrap() = json!(path);
    config["models"].as_array_mut().unwrap().push(model);
}

async fn load(server: &Server, model: &str) -> (StatusCode, Value) {
    server.post("load", json!({"model": model})).await
}

async fn chat(server: &Server, model: &str, id: &str) -> StatusCode {
    server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model": model, "test_id": id})),
            server.read_key(),
        )
        .await
        .0
}

async fn unload_lines(server: &Server, reason: &str) -> Vec<String> {
    server.get("juiced/logs?limit=500").await["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|line| line["text"].as_str())
        .filter(|text| text.ends_with(&format!("unloaded (reason: {reason})")))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn idle_on_demand_model_unloads_after_its_delay_and_records_the_reason() {
    let server = Server::configured(true, idle(400)).await;
    assert_eq!(
        server.get("juiced/state").await["residency"]["idle_unload_after_ms"],
        400
    );
    assert_eq!(load(&server, "fixture.one").await.0, StatusCode::OK);
    let loaded = server.model("fixture.one").await;
    let residency = &loaded["residency"];
    assert_eq!(residency["idle_unload_after_ms"], 400);
    assert_eq!(residency["source"], "service");
    assert_eq!(residency["model_idle_unload_after_ms"], Value::Null);
    let used = residency["last_used_unix_ms"].as_u64().unwrap();
    assert_eq!(residency["idle_unload_at_unix_ms"], used + 400);
    let detail = server.get("models/fixture.one").await;
    assert_eq!(detail["juiced"]["residency"]["idle_unload_after_ms"], 400);
    server
        .wait_for(async || {
            !server.model("fixture.one").await["loaded"]
                .as_bool()
                .unwrap()
        })
        .await;
    let unloaded = server.model("fixture.one").await;
    assert_eq!(unloaded["residency"]["last_unload"]["reason"], "idle");
    assert_eq!(unloaded["residency"]["idle_unload_at_unix_ms"], Value::Null);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(unload_lines(&server, "idle").await.len(), 1);
    assert_eq!(
        chat(&server, "fixture.one", "after-idle").await,
        StatusCode::OK
    );
    assert!(
        server.model("fixture.one").await["loaded"]
            .as_bool()
            .unwrap()
    );
}

#[tokio::test]
async fn hot_models_never_idle_unload() {
    let server = Server::configured(false, |config| {
        config["idle_unload_after_ms"] = json!(100);
        config["models"][0]["policy"] = json!("hot");
    })
    .await;
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == true)
        .await;
    let pid = server.model("fixture.one").await["pid"].clone();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let model = server.model("fixture.one").await;
    assert_eq!(model["pid"], pid);
    assert_eq!(model["residency"]["idle_unload_at_unix_ms"], Value::Null);
    assert!(unload_lines(&server, "idle").await.is_empty());
}

#[tokio::test]
async fn busy_model_unloads_only_after_its_last_request_settles() {
    let server = Server::configured(false, idle(300)).await;
    let request = server
        .socket_request(
            "chat/completions",
            json!({"model":"fixture.one","test_id":"long","fixture_delay_ms":1200}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "long", "started"))
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let busy = server.model("fixture.one").await;
    assert_eq!(busy["loaded"], true);
    assert_eq!(busy["active_requests"], 1);
    assert_eq!(busy["residency"]["idle_unload_at_unix_ms"], Value::Null);
    server
        .wait_for(async || server.event("fixture.one", "long", "completed"))
        .await;
    drop(request);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let settled = server.model("fixture.one").await;
    if settled["loaded"] == true {
        let used = settled["residency"]["last_used_unix_ms"].as_u64().unwrap();
        assert_eq!(settled["residency"]["idle_unload_at_unix_ms"], used + 300);
    }
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == false)
        .await;
    assert_eq!(
        server.model("fixture.one").await["residency"]["last_unload"]["reason"],
        "idle"
    );
}

#[tokio::test]
async fn pressure_evicts_the_least_recently_used_idle_model_and_the_load_succeeds() {
    let server = Server::configured(false, |config| add_model(config, "fixture.three")).await;
    assert_eq!(load(&server, "fixture.one").await.0, StatusCode::OK);
    assert_eq!(load(&server, "fixture.two").await.0, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(chat(&server, "fixture.one", "recent").await, StatusCode::OK);
    let pid = server.model("fixture.one").await["pid"].clone();
    let (status, loaded) = load(&server, "fixture.three").await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["loaded"], true);
    let two = server.model("fixture.two").await;
    assert_eq!(two["loaded"], false);
    assert_eq!(two["residency"]["last_unload"]["reason"], "pressure");
    assert_eq!(server.model("fixture.one").await["pid"], pid);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        128
    );
    assert_eq!(unload_lines(&server, "pressure").await.len(), 1);
    assert_eq!(chat(&server, "fixture.two", "reload").await, StatusCode::OK);
    assert_eq!(
        server.model("fixture.one").await["residency"]["last_unload"]["reason"],
        "pressure"
    );
    let (status, _) = server
        .post("unload", json!({"model":"fixture.three"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        server.model("fixture.three").await["residency"]["last_unload"]["reason"],
        "operator"
    );
    assert_eq!(unload_lines(&server, "operator").await.len(), 1);
}

#[tokio::test]
async fn pressure_without_an_evictable_model_names_what_holds_memory() {
    let server = Server::configured(false, |config| {
        config["resident_budget_bytes"] = json!(128);
        add_model(config, "fixture.three");
        config["models"][0]["policy"] = json!("hot");
    })
    .await;
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == true)
        .await;
    let busy = server
        .socket_request(
            "chat/completions",
            json!({"model":"fixture.two","test_id":"hold","fixture_delay_ms":2000}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.two", "hold", "started"))
        .await;
    let (status, error) = load(&server, "fixture.three").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "resident_budget");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("fixture.one (hot)"), "{message}");
    assert!(message.contains("fixture.two (busy)"), "{message}");
    assert_eq!(server.model("fixture.two").await["loaded"], true);
    assert!(unload_lines(&server, "pressure").await.is_empty());
    drop(busy);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_loads_evict_each_idle_model_once_without_overcommit() {
    let server = Server::configured(false, |config| {
        add_model(config, "fixture.three");
        add_model(config, "fixture.four");
    })
    .await;
    assert_eq!(load(&server, "fixture.one").await.0, StatusCode::OK);
    assert_eq!(load(&server, "fixture.two").await.0, StatusCode::OK);
    let (three, four) = tokio::join!(
        load(&server, "fixture.three"),
        load(&server, "fixture.four")
    );
    assert_eq!(three.0, StatusCode::OK, "{}", three.1);
    assert_eq!(four.0, StatusCode::OK, "{}", four.1);
    let catalog = server.get("juiced/catalog").await;
    let loaded = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|model| model["loaded"] == true)
        .map(|model| model["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(loaded, ["fixture.four", "fixture.three"]);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        128
    );
    let evictions = unload_lines(&server, "pressure").await;
    assert_eq!(evictions.len(), 2, "{evictions:?}");
    assert!(evictions.iter().any(|line| line.contains("fixture.one ")));
    assert!(evictions.iter().any(|line| line.contains("fixture.two ")));
}

#[tokio::test]
async fn residency_route_publishes_service_and_model_delays() {
    let server = Server::configured(true, |_| {}).await;
    let view = server.get("juiced/residency").await;
    assert_eq!(view["schema_version"], 1);
    assert_eq!(view["idle_unload_after_ms"], 900000);
    let generation = view["configuration_generation"].as_u64().unwrap();
    let (status, error) = server
        .post(
            "juiced/residency",
            json!({"expected_generation": generation + 1, "idle_unload_after_ms": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "revision_conflict");
    for invalid in [
        json!({"expected_generation": generation}),
        json!({"expected_generation": generation, "idle_unload_after_ms": null}),
        json!({"expected_generation": generation, "idle_unload_after_ms": 2_592_000_001u64}),
    ] {
        let (status, error) = server.post("juiced/residency", invalid).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid_residency");
    }
    let (status, error) = server
        .post(
            "juiced/residency",
            json!({"expected_generation": generation, "model": "missing", "idle_unload_after_ms": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "unknown_model");
    let (status, view) = server
        .post(
            "juiced/residency",
            json!({"expected_generation": generation, "idle_unload_after_ms": 300}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["idle_unload_after_ms"], 300);
    let generation = view["configuration_generation"].as_u64().unwrap();
    let (status, view) = server
        .post(
            "juiced/residency",
            json!({"expected_generation": generation, "model": "fixture.one", "idle_unload_after_ms": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let row = view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "fixture.one")
        .unwrap();
    assert_eq!(row["residency"]["model_idle_unload_after_ms"], 0);
    assert_eq!(row["residency"]["idle_unload_after_ms"], Value::Null);
    assert_eq!(row["residency"]["source"], "model");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(saved["idle_unload_after_ms"], 300);
    assert_eq!(
        saved["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == "fixture.one")
            .unwrap()["idle_unload_after_ms"],
        0
    );
    assert_eq!(load(&server, "fixture.one").await.0, StatusCode::OK);
    assert_eq!(load(&server, "fixture.two").await.0, StatusCode::OK);
    server
        .wait_for(async || server.model("fixture.two").await["loaded"] == false)
        .await;
    assert_eq!(server.model("fixture.one").await["loaded"], true);
    let generation = server.get("juiced/residency").await["configuration_generation"]
        .as_u64()
        .unwrap();
    let (status, view) = server
        .post(
            "juiced/residency",
            json!({"expected_generation": generation, "model": "fixture.one", "idle_unload_after_ms": null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == false)
        .await;
    let (status, _) = server
        .json(Method::GET, "/v1/juiced/residency", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ollama_keep_alive_overrides_the_idle_delay_until_unload() {
    let server = Server::configured(false, idle(300)).await;
    let ollama = async |keep_alive: Value| {
        server
            .json(
                Method::POST,
                "/api/chat",
                Some(json!({"model":"fixture.one","messages":[{"role":"user","content":"hi"}],"stream":false,"keep_alive":keep_alive})),
                server.read_key(),
            )
            .await
    };
    let (status, error) = ollama(json!("soon")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    let (status, reply) = ollama(json!(-1)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let model = server.model("fixture.one").await;
    assert_eq!(model["residency"]["source"], "keep_alive");
    assert_eq!(model["residency"]["keep_alive_ms"], -1);
    assert_eq!(model["residency"]["idle_unload_after_ms"], Value::Null);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(server.model("fixture.one").await["loaded"], true);
    let (status, reply) = ollama(json!("1m")).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        server.model("fixture.one").await["residency"]["idle_unload_after_ms"],
        60000
    );
    let (status, reply) = server
        .json(
            Method::POST,
            "/api/generate",
            Some(json!({"model":"fixture.one","prompt":"bye","stream":false,"keep_alive":0})),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == false)
        .await;
    let model = server.model("fixture.one").await;
    assert_eq!(model["residency"]["last_unload"]["reason"], "idle");
    assert_eq!(model["residency"]["source"], "service");
    assert_eq!(model["residency"]["keep_alive_ms"], Value::Null);
}

#[tokio::test]
async fn idle_unload_saves_unsaved_session_snapshots_first() {
    let server = Server::configured(true, |config| {
        config["idle_unload_after_ms"] = json!(400);
        config["session_cache"] = json!({"save_delay_ms": 60000});
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["options"] = json!({"parallel_slots": 1, "context_tokens": 512});
        model["args"]
            .as_array_mut()
            .unwrap()
            .push(json!("--emulate-prompt-cache"));
    })
    .await;
    let messages = json!([
        {"role":"system","content":"Tide specialist: discuss with care and precision."},
        {"role":"user","content":"Tell me about tides ".repeat(40)},
    ]);
    let response = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .bearer_auth(server.read_key().unwrap())
        .header("x-juiced-thread-id", "thread-a")
        .json(&json!({"model":"fixture.one","messages":messages,"stream":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let row = |view: &Value| {
        view["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["model"] == "fixture.one")
            .unwrap()
            .clone()
    };
    let view = server.get("juiced/cache/sessions").await;
    assert_eq!(row(&view)["slots"][0]["state"], "unsaved", "{view}");
    assert_eq!(view["totals"]["sessions"], 0, "{view}");
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == false)
        .await;
    assert_eq!(
        server.model("fixture.one").await["residency"]["last_unload"]["reason"],
        "idle"
    );
    let view = server.get("juiced/cache/sessions").await;
    assert_eq!(view["totals"]["sessions"], 1, "{view}");
    assert_eq!(view["sessions"][0]["kind"], "header", "{view}");
}
