#![cfg(target_os = "linux")]

mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server};

const MODEL: &str = "fixture.one";

fn configure(config: &mut Value, slots: u32, settings: Value) {
    config["session_cache"] = settings;
    let model = &mut config["models"][0];
    model["provider"] = json!("llama-cpp");
    model["max_inflight"] = json!(slots);
    model["options"] = json!({"parallel_slots": slots, "context_tokens": 512});
    model["args"]
        .as_array_mut()
        .unwrap()
        .push(json!("--emulate-prompt-cache"));
}

async fn session_server(slots: u32, settings: Value) -> Server {
    Server::configured(true, |config| configure(config, slots, settings)).await
}

fn fast() -> Value {
    json!({"save_delay_ms": 20})
}

fn conversation(topic: &str) -> Vec<Value> {
    vec![
        json!({"role":"system","content":format!("{topic} specialist: discuss with care and precision.")}),
        json!({"role":"user","content":format!("Tell me {}", format!("about {topic} ").repeat(40))}),
    ]
}

async fn chat(server: &Server, messages: &[Value], header: Option<(&str, &str)>) -> Value {
    let mut request = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .json(&json!({"model":MODEL,"messages":messages,"stream":false}));
    if let Some(key) = server.read_key() {
        request = request.bearer_auth(key);
    }
    if let Some((name, value)) = header {
        request = request.header(name, value);
    }
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn turn(server: &Server, messages: &mut Vec<Value>, question: &str) -> Value {
    messages.push(json!({"role":"user","content":question}));
    let reply = chat(server, messages, None).await;
    messages.push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
    reply
}

async fn sessions(server: &Server) -> Value {
    server.get("juiced/cache/sessions").await
}

async fn settled(server: &Server, expected: usize) -> Value {
    let mut last = Value::Null;
    for _ in 0..300 {
        last = sessions(server).await;
        let idle = model_row(&last)["slots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|slot| slot["state"] != "unsaved" && slot["state"] != "busy");
        if idle && last["totals"]["sessions"] == expected {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("session snapshots did not settle at {expected}: {last}");
}

fn model_row(view: &Value) -> &Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model"] == MODEL)
        .unwrap()
}

async fn last_record(server: &Server) -> Value {
    server.get("juiced/requests").await["data"][0].clone()
}

fn processed(reply: &Value) -> u64 {
    reply["timings"]["prompt_n"].as_u64().unwrap()
}

fn cached(reply: &Value) -> u64 {
    reply["timings"]["cache_n"].as_u64().unwrap()
}

#[tokio::test]
async fn older_conversation_restores_on_a_shared_slot_and_after_service_restart() {
    let mut server = session_server(1, fast()).await;
    let mut a = conversation("rivers");
    let first = chat(&server, &a, None).await;
    assert_eq!(cached(&first), 0);
    assert_eq!(last_record(&server).await["session_cache"]["path"], "cold");
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    let view = settled(&server, 1).await;
    assert_eq!(view["sessions"][0]["kind"], "prefix");
    assert_eq!(view["sessions"][0]["model"], MODEL);
    assert_eq!(view["sessions"][0]["execution_current"], true);
    assert_eq!(view["contains_content"], false);
    assert!(!view.to_string().contains("rivers"));

    let mut b = conversation("mountains");
    let other = chat(&server, &b, None).await;
    assert!(cached(&other) <= 1);
    b.push(json!({"role":"assistant","content":other["choices"][0]["message"]["content"]}));
    settled(&server, 2).await;

    let restored = turn(&server, &mut a, "And then?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(record["session_cache"]["restore_ms"].is_u64());
    assert_eq!(record["cached_tokens"], cached(&restored));
    assert!(processed(&restored) <= 4, "{restored}");
    assert!(cached(&restored) > 80, "{restored}");
    let view = settled(&server, 2).await;
    assert_eq!(model_row(&view)["effective"], "persistent");

    let hit = turn(&server, &mut a, "Continue.").await;
    assert_eq!(
        last_record(&server).await["session_cache"]["path"],
        "slot-hit"
    );
    assert!(processed(&hit) <= 4);
    settled(&server, 2).await;

    server.restart().await;
    let after_restart = turn(&server, &mut a, "Once more.").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&after_restart) <= 4, "{after_restart}");
    settled(&server, 2).await;
    let back = turn(&server, &mut b, "Still there?").await;
    assert_eq!(
        last_record(&server).await["session_cache"]["path"],
        "restored"
    );
    assert!(processed(&back) <= 4);
    let rows = settled(&server, 2).await;
    assert!(
        rows["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["bytes"].as_u64().unwrap() > 0)
    );
}

#[tokio::test]
async fn header_sessions_take_precedence_and_unload_flushes_unsaved_state() {
    let server = session_server(1, json!({"save_delay_ms": 60000})).await;
    let a = conversation("tides");
    let b = conversation("forests");
    chat(&server, &a, Some(("x-juiced-thread-id", "thread-a"))).await;
    assert_eq!(last_record(&server).await["session_cache"]["key"], "header");
    let view = sessions(&server).await;
    assert_eq!(model_row(&view)["slots"][0]["state"], "unsaved");
    chat(&server, &b, Some(("x-juiced-session-id", "thread-b"))).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["saved_previous"], true, "{record}");
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    let view = settled(&server, 2).await;
    assert!(
        view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] == "header")
    );
    let mut follow = conversation("unrelated text, same thread");
    follow.push(json!({"role":"user","content":"next"}));
    let reply = chat(&server, &follow, Some(("x-juiced-thread-id", "thread-a"))).await;
    assert_eq!(
        last_record(&server).await["session_cache"]["path"],
        "restored"
    );
    assert!(
        cached(&reply) <= 1,
        "header restore never forces a mismatched prefix"
    );
    let duplicate = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .bearer_auth(server.read_key().unwrap())
        .header("x-juiced-session-id", "one")
        .header("x-juiced-session-id", "two")
        .json(&json!({"model":MODEL,"messages":a}))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_count_budget_evicts_least_recent_and_disabled_prompt_cache_bypasses() {
    let server = session_server(1, json!({"save_delay_ms": 20, "max_sessions": 2})).await;
    let mut a = conversation("glaciers");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    settled(&server, 1).await;
    for topic in ["deserts", "oceans"] {
        chat(&server, &conversation(topic), None).await;
    }
    settled(&server, 2).await;
    let evicted = turn(&server, &mut a, "Remember?").await;
    assert_eq!(last_record(&server).await["session_cache"]["path"], "cold");
    assert!(cached(&evicted) <= 1);
    settled(&server, 2).await;
    let response = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .bearer_auth(server.read_key().unwrap())
        .json(&json!({"model":MODEL,"messages":conversation("deserts"),"cache_prompt":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        last_record(&server).await["session_cache"]["path"],
        "bypass"
    );
}

#[tokio::test]
async fn changed_execution_identity_discards_old_snapshots_and_rebuilds_cold() {
    let server = session_server(1, fast()).await;
    let mut a = conversation("volcanoes");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    settled(&server, 1).await;
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    let before = server.get(&format!("models/{MODEL}/options")).await;
    let (status, _) = server
        .post(
            &format!("models/{MODEL}/options"),
            json!({"expected_revision":before["revision"],"options":{"parallel_slots":1,"context_tokens":1024}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let reply = turn(&server, &mut a, "After the change?").await;
    assert_eq!(last_record(&server).await["session_cache"]["path"], "cold");
    assert!(cached(&reply) <= 1);
    let view = settled(&server, 1).await;
    assert_eq!(view["sessions"][0]["execution_current"], true);
}

#[tokio::test]
async fn corrupted_snapshot_is_discarded_and_the_turn_rebuilds_cold() {
    let server = session_server(1, fast()).await;
    let mut a = conversation("estuaries");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    settled(&server, 1).await;
    chat(&server, &conversation("plateaus"), None).await;
    settled(&server, 2).await;
    for entry in std::fs::read_dir(server.directory.path().join("kv-cache")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "bin") {
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[0] ^= 1;
            std::fs::write(&path, bytes).unwrap();
        }
    }
    let reply = turn(&server, &mut a, "Still intact?").await;
    assert_eq!(last_record(&server).await["session_cache"]["path"], "cold");
    assert!(cached(&reply) <= 1);
    let view = settled(&server, 2).await;
    assert!(
        view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "saved")
    );
}

#[tokio::test]
async fn anthropic_gateway_conversations_restore_through_the_same_sessions() {
    let server = session_server(1, fast()).await;
    async fn message(server: &Server, topic: &str, messages: &[Value]) -> Value {
        let (status, reply) = server
            .json(
                Method::POST,
                "/v1/messages",
                Some(json!({"model":MODEL,"system":format!("{topic} specialist."),"messages":messages,"max_tokens":8})),
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        reply
    }
    let question = json!({"role":"user","content":"Describe it ".repeat(30)});
    let first = message(&server, "harbors", std::slice::from_ref(&question)).await;
    settled(&server, 1).await;
    message(&server, "canyons", std::slice::from_ref(&question)).await;
    settled(&server, 2).await;
    let follow = [
        question,
        json!({"role":"assistant","content":first["content"][0]["text"]}),
        json!({"role":"user","content":"More?"}),
    ];
    message(&server, "harbors", &follow).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(record["cached_tokens"].as_u64().unwrap() > 30, "{record}");
    assert!(record["prompt_processed_tokens"].as_u64().unwrap() <= 2);
}

#[tokio::test]
async fn multiple_slots_keep_conversations_resident_with_engine_slot_routing() {
    let server = session_server(2, json!({"mode":"memory"})).await;
    let mut a = conversation("comets");
    let mut b = conversation("planets");
    turn(&server, &mut a, "First?").await;
    let a_slot = last_record(&server).await["session_cache"]["slot"].clone();
    turn(&server, &mut b, "First?").await;
    let b_slot = last_record(&server).await["session_cache"]["slot"].clone();
    assert_ne!(a_slot, b_slot);
    let hit = turn(&server, &mut a, "Second?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "slot-hit");
    assert_eq!(record["session_cache"]["slot"], a_slot);
    assert!(processed(&hit) <= 4);
    assert_eq!(hit["timings"]["id_slot"], a_slot);
    let view = sessions(&server).await;
    assert_eq!(model_row(&view)["effective"], "memory", "{view}");
    assert_eq!(view["totals"]["sessions"], 0);
}

#[tokio::test]
async fn session_routes_configure_forget_and_own_explicit_slot_operations() {
    let server = session_server(1, fast()).await;
    for prefix in PREFIXES {
        let (status, view) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/cache/sessions"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["settings"]["mode"], "persistent");
        let (status, _) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/cache/sessions"),
                None,
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    chat(&server, &conversation("lakes"), None).await;
    settled(&server, 1).await;
    let (status, error) = server
        .post(
            "juiced/cache/save",
            json!({"model":MODEL,"slot":0,"session":"manual","expected_generation":0,"maximum_bytes":1024}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "cache_sessions_managed");
    let (status, error) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"forget","session":"not-a-digest"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_session");
    let (status, forgotten) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"forget","model":MODEL}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(forgotten["forgotten"], 1);
    assert_eq!(sessions(&server).await["totals"]["sessions"], 0);
    let generation = sessions(&server).await["configuration_generation"].clone();
    let (status, error) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"configure","expected_generation":generation,"settings":{"model_budget_bytes":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_session_settings");
    let (status, _) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"configure","expected_generation":generation,"settings":{"models":{MODEL:"off"}}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, error) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"configure","expected_generation":generation,"settings":{}}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "revision_conflict");
    let view = sessions(&server).await;
    assert_eq!(model_row(&view)["effective"], "off");
    chat(&server, &conversation("lakes"), None).await;
    assert!(last_record(&server).await.get("session_cache").is_none());
    let (status, error) = server
        .post(
            "juiced/cache/save",
            json!({"model":MODEL,"slot":0,"session":"manual","expected_generation":0,"maximum_bytes":1024}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "cache_disabled");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(saved["session_cache"]["models"][MODEL], "off");
}

#[tokio::test]
async fn removing_a_model_forgets_its_saved_sessions() {
    let server = session_server(1, fast()).await;
    chat(&server, &conversation("rivers"), None).await;
    chat(&server, &conversation("glaciers"), None).await;
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    settled(&server, 2).await;
    let catalog = server.get("juiced/catalog").await;
    let model = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == MODEL)
        .unwrap()
        .clone();
    let (status, disabled) = server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"disabled","expected_revision":model["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":MODEL,"expected_revision":disabled["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["sessions_forgotten"], 2, "{removed}");
    let view = sessions(&server).await;
    assert_eq!(view["totals"]["sessions"], 0, "{view}");
}

fn shaped_gguf() -> Vec<u8> {
    fn string(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend((text.len() as u64).to_le_bytes());
        bytes.extend(text.as_bytes());
    }
    let numbers = [
        ("llama.context_length", 4096u32),
        ("llama.block_count", 1),
        ("llama.embedding_length", 2),
        ("llama.attention.head_count", 1),
        ("llama.attention.head_count_kv", 1),
        ("llama.attention.key_length", 1),
        ("llama.attention.value_length", 1),
    ];
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend((numbers.len() as u64 + 1).to_le_bytes());
    string(&mut bytes, "general.architecture");
    bytes.extend(8u32.to_le_bytes());
    string(&mut bytes, "llama");
    for (key, value) in numbers {
        string(&mut bytes, key);
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

fn add_model(config: &mut Value, id: &str) {
    let mut model = config["models"][1].clone();
    model["id"] = json!(id);
    let args = model["args"].as_array_mut().unwrap();
    let events = args
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .replace("fixture.two", id);
    *args.last_mut().unwrap() = json!(events);
    config["models"].as_array_mut().unwrap().push(model);
}

async fn unload_reason(server: &Server) -> Value {
    server.model(MODEL).await["residency"]["last_unload"]["reason"].clone()
}

#[tokio::test]
async fn checkpoint_heavy_slot_state_grows_past_its_kv_estimate_and_still_restores() {
    const OVERHEAD: u64 = 72 * 1024 * 1024;
    let server = Server::configured(true, |config| {
        configure(config, 1, fast());
        config["request_timeout_ms"] = json!(30000);
        let shaped = std::path::Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .with_file_name("shaped.gguf");
        std::fs::write(&shaped, shaped_gguf()).unwrap();
        config["models"][0]["model_file"] = json!(shaped);
        let args = config["models"][0]["args"].as_array_mut().unwrap();
        args.extend([
            json!("--slot-state-overhead-bytes"),
            json!(OVERHEAD.to_string()),
        ]);
    })
    .await;
    let mut a = conversation("reefs");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    let view = settled(&server, 1).await;
    assert_eq!(view["sessions"][0]["state"], "saved", "{view}");
    assert!(view["sessions"][0]["bytes"].as_u64().unwrap() > OVERHEAD);
    chat(&server, &conversation("dunes"), None).await;
    settled(&server, 2).await;
    turn(&server, &mut a, "Deeper?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    let warnings = server.get("juiced/logs?limit=500").await.to_string();
    assert!(!warnings.contains("was not saved"), "{warnings}");
    let view = settled(&server, 2).await;
    assert!(
        view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "saved"),
        "{view}"
    );
}

#[tokio::test]
async fn returning_conversations_restore_after_options_reload_and_pressure_eviction() {
    let server = Server::configured(true, |config| {
        configure(config, 1, json!({"save_delay_ms": 60000}));
        config["resident_budget_bytes"] = json!(128);
        add_model(config, "fixture.three");
    })
    .await;
    let mut a = conversation("harvests");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    assert_eq!(
        model_row(&sessions(&server).await)["slots"][0]["state"],
        "unsaved"
    );
    let pid = server.model(MODEL).await["pid"].clone();
    let before = server.get(&format!("models/{MODEL}/options")).await;
    let (status, reloaded) = server
        .post(
            &format!("models/{MODEL}/options"),
            json!({"expected_revision":before["revision"],"options":before["saved"],"apply":"reload"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(reloaded["restarted"], true, "{reloaded}");
    assert_ne!(server.model(MODEL).await["pid"], pid);
    let reply = turn(&server, &mut a, "Reloaded?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&reply) <= 4, "{reply}");

    for model in ["fixture.two", "fixture.three"] {
        let (status, loaded) = server.post("load", json!({"model":model})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
    }
    assert_eq!(server.model(MODEL).await["loaded"], false);
    assert_eq!(unload_reason(&server).await, "pressure");
    let reply = turn(&server, &mut a, "Evicted?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&reply) <= 4, "{reply}");
}

#[tokio::test]
async fn returning_conversations_restore_after_idle_unload() {
    let server = Server::configured(true, |config| {
        configure(config, 1, fast());
        config["idle_unload_after_ms"] = json!(600);
    })
    .await;
    let mut a = conversation("orchards");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    server
        .wait_for(async || server.model(MODEL).await["loaded"] == false)
        .await;
    assert_eq!(unload_reason(&server).await, "idle");
    assert_eq!(sessions(&server).await["totals"]["sessions"], 1);
    let reply = turn(&server, &mut a, "Later?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&reply) <= 4, "{reply}");
}

#[tokio::test]
async fn unload_waits_for_a_save_in_flight_instead_of_stranding_it() {
    let server = Server::configured(true, |config| {
        configure(config, 1, fast());
        let args = config["models"][0]["args"].as_array_mut().unwrap();
        args.extend([json!("--slot-operation-delay-ms"), json!("1500")]);
    })
    .await;
    chat(&server, &conversation("meadows"), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    let view = sessions(&server).await;
    assert_eq!(view["totals"]["sessions"], 1, "{view}");
    assert_eq!(view["sessions"][0]["state"], "saved", "{view}");
}
