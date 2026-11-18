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

fn new_conversation(system: &str, question: &str) -> Vec<Value> {
    vec![
        json!({"role":"system","content":system}),
        json!({"role":"user","content":question}),
    ]
}

#[tokio::test]
async fn new_conversations_restore_a_saved_state_that_shares_their_system_prompt() {
    let mut server = session_server(1, fast()).await;
    let assistant = "Household assistant with these exposed devices: ".to_owned()
        + &"porch light, hallway sensor, garage door, ".repeat(60);
    let first = chat(
        &server,
        &new_conversation(&assistant, "Turn on the porch."),
        None,
    )
    .await;
    assert_eq!(cached(&first), 0);
    settled(&server, 1).await;
    let pirate = "Pirate storyteller who answers in rhyme. ".repeat(60);
    chat(&server, &new_conversation(&pirate, "A tale?"), None).await;
    settled(&server, 2).await;

    let system_words = assistant.split_whitespace().count() as u64;
    let returning = chat(
        &server,
        &new_conversation(&assistant, "Is the garage open?"),
        None,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&returning) > system_words, "{returning}");
    assert!(processed(&returning) <= 6, "{returning}");
    settled(&server, 2).await;

    server.restart().await;
    let after_restart = chat(
        &server,
        &new_conversation(&assistant, "Lock the hallway?"),
        None,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&after_restart) > system_words, "{after_restart}");
    settled(&server, 2).await;

    let brief = "Be brief.";
    chat(&server, &new_conversation(brief, "One?"), None).await;
    settled(&server, 3).await;
    chat(&server, &new_conversation(&pirate, "Another tale?"), None).await;
    settled(&server, 3).await;
    let short = chat(&server, &new_conversation(brief, "Two?"), None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "cold", "{record}");
    assert!(cached(&short) <= 1, "{short}");
}

fn devices() -> String {
    "Household assistant with these exposed devices: ".to_owned()
        + &"porch light, hallway sensor, garage door, ".repeat(60)
}

fn pirate() -> String {
    "Pirate storyteller who answers in rhyme. ".repeat(60)
}

async fn retain(server: &Server, session: &Value) {
    let (status, retained) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"retain","session":session,"retained":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{retained}");
}

#[tokio::test]
async fn one_off_requests_that_share_a_long_opening_leave_one_state() {
    let server = session_server(1, fast()).await;
    let system = devices();
    let questions = [
        "Turn on the porch.",
        "Is the garage open?",
        "Lock the hallway.",
        "Dim the porch light.",
        "Any motion in the hallway?",
        "Close the garage.",
    ];
    let mut sizes = Vec::new();
    for question in questions {
        chat(&server, &new_conversation(&system, question), None).await;
        let view = settled(&server, 1).await;
        sizes.push(view["totals"]["bytes"].as_u64().unwrap());
    }
    assert!(sizes[questions.len() - 1] < 2 * sizes[0], "{sizes:?}");

    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    settled(&server, 2).await;
    let system_words = system.split_whitespace().count() as u64;
    let mut latest = new_conversation(&system, "Is the porch on now?");
    let sibling = chat(&server, &latest, None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&sibling) > system_words, "{sibling}");
    assert!(processed(&sibling) <= 8, "{sibling}");
    latest.push(json!({"role":"assistant","content":sibling["choices"][0]["message"]["content"]}));
    settled(&server, 2).await;

    chat(&server, &new_conversation(&pirate(), "Another tale?"), None).await;
    settled(&server, 2).await;
    let continued = turn(&server, &mut latest, "And the garage?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&continued) > system_words, "{continued}");
    assert!(processed(&continued) <= 6, "{continued}");
    settled(&server, 2).await;
}

#[tokio::test]
async fn continued_conversations_and_small_shared_openings_are_each_kept() {
    let mut server = session_server(1, fast()).await;
    let system = devices();
    let mut first = new_conversation(&system, "Turn on the porch.");
    let reply = chat(&server, &first, None).await;
    first.push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
    let follow = "Thanks. And the hallway? ".to_owned() + &"Check each sensor twice. ".repeat(16);
    turn(&server, &mut first, &follow).await;
    settled(&server, 1).await;
    let mut second = new_conversation(&system, "Is the garage open?");
    let reply = chat(&server, &second, None).await;
    second.push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
    settled(&server, 2).await;
    turn(&server, &mut second, "Close it.").await;
    settled(&server, 2).await;

    chat(&server, &new_conversation(&system, "Any motion?"), None).await;
    settled(&server, 3).await;
    chat(&server, &new_conversation(&system, "Dim the porch."), None).await;
    settled(&server, 3).await;

    server.restart().await;
    chat(
        &server,
        &new_conversation(&system, "Lock the hallway."),
        None,
    )
    .await;
    settled(&server, 3).await;
    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    settled(&server, 4).await;
    let returning = turn(&server, &mut first, "Still on?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&returning) <= 6, "{returning}");
    settled(&server, 4).await;

    let brief = "Answer in one short sentence. ".repeat(30);
    assert!(brief.len() < 1024);
    for (count, question) in ["One?", "Two?", "Three?"].into_iter().enumerate() {
        chat(&server, &new_conversation(&brief, question), None).await;
        settled(&server, 5 + count).await;
    }

    let rules = "Reviewer rules: be exact, cite lines. ".repeat(60);
    for (count, topic) in ["rivers", "glaciers"].into_iter().enumerate() {
        let draft =
            format!("Review this draft about {topic}: ") + &format!("{topic} matter. ").repeat(30);
        chat(&server, &new_conversation(&rules, &draft), None).await;
        settled(&server, 8 + count).await;
    }
}

#[tokio::test]
async fn a_request_that_already_holds_a_reply_is_a_conversation_and_is_kept() {
    let server = session_server(1, fast()).await;
    let system = devices();
    let mut history = new_conversation(&system, "Turn on the porch.");
    history.push(json!({"role":"assistant","content":"The porch is on."}));
    turn(&server, &mut history, "And the hallway?").await;
    settled(&server, 1).await;
    chat(&server, &new_conversation(&system, "Any motion?"), None).await;
    settled(&server, 2).await;
    chat(&server, &new_conversation(&system, "Dim the porch."), None).await;
    settled(&server, 2).await;
    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    settled(&server, 3).await;
    let reply = turn(&server, &mut history, "Thanks.").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&reply) <= 4, "{reply}");
    // The conversation still opens with the same messages for nine tenths of its text, so its
    // new turn replaces the one-off request's state.
    settled(&server, 2).await;
}

#[tokio::test]
async fn a_kept_state_is_not_replaced_by_a_sibling_or_by_its_own_next_turn() {
    let server = session_server(1, fast()).await;
    let system = devices();
    let mut pinned = new_conversation(&system, "Turn on the porch.");
    let reply = chat(&server, &pinned, None).await;
    pinned.push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
    let view = settled(&server, 1).await;
    let kept = view["sessions"][0]["session"].clone();
    retain(&server, &kept).await;

    chat(
        &server,
        &new_conversation(&system, "Is the garage open?"),
        None,
    )
    .await;
    settled(&server, 2).await;
    chat(&server, &new_conversation(&system, "Any motion?"), None).await;
    let view = settled(&server, 2).await;
    assert!(
        view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["session"] == kept && row["retained"] == true),
        "{view}"
    );

    turn(&server, &mut pinned, "And the hallway?").await;
    let view = settled(&server, 2).await;
    let rows = view["sessions"].as_array().unwrap();
    let pinned_bytes = rows
        .iter()
        .find(|row| row["session"] == kept && row["retained"] == true)
        .unwrap_or_else(|| panic!("{view}"))["bytes"]
        .as_u64();
    assert!(
        rows.iter()
            .any(|row| row["retained"] == false && row["bytes"].as_u64() > pinned_bytes),
        "{view}"
    );
}

#[tokio::test]
async fn model_checks_save_no_state_and_leave_client_conversations_restorable() {
    let server = session_server(1, fast()).await;
    let mut a = conversation("orchards");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    settled(&server, 1).await;

    // Tune refuses this engine at once and then runs the automatic check with service authority.
    let (status, started) = server
        .post(&format!("models/{MODEL}/tune"), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let mut automatic = Value::Null;
    server
        .wait_for(async || {
            automatic = server.get("juiced/requests").await["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|record| record["protocol"] == "internal")
                .cloned()
                .unwrap_or(Value::Null);
            !automatic.is_null()
        })
        .await;
    assert_eq!(
        automatic["session_cache"]["key"],
        Value::Null,
        "{automatic}"
    );

    let (status, checked) = server
        .post(&format!("models/{MODEL}/check"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    let records = server.get("juiced/requests").await;
    let checks = records["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["id"] != 1)
        .collect::<Vec<_>>();
    assert!(checks.len() >= 2, "{records}");
    for record in checks {
        assert_eq!(record["session_cache"]["key"], Value::Null, "{record}");
    }
    let view = settled(&server, 1).await;
    assert_ne!(
        view["sessions"][0]["principal"], "juiced-internal",
        "{view}"
    );
    assert_eq!(model_row(&view)["slots"][0]["state"], "empty", "{view}");

    let reply = turn(&server, &mut a, "After the check?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&reply) <= 4, "{reply}");
}

async fn set_context(server: &Server, tokens: u32) {
    let before = server.get(&format!("models/{MODEL}/options")).await;
    let (status, saved) = server
        .post(
            &format!("models/{MODEL}/options"),
            json!({"expected_revision":before["revision"],"options":{"parallel_slots":1,"context_tokens":tokens}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

#[tokio::test]
async fn states_made_under_other_engine_settings_go_when_the_model_starts_and_kept_ones_stay() {
    let server = session_server(1, fast()).await;
    chat(&server, &conversation("volcanoes"), None).await;
    settled(&server, 1).await;
    let mut kept = conversation("glaciers");
    let first = chat(&server, &kept, None).await;
    kept.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    let view = settled(&server, 2).await;
    let pinned = view["sessions"][0]["session"].clone();
    retain(&server, &pinned).await;

    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    set_context(&server, 1024).await;
    let view = sessions(&server).await;
    assert_eq!(view["totals"]["sessions"], 2, "{view}");
    for row in view["sessions"].as_array().unwrap() {
        assert_eq!(row["execution_current"], Value::Null, "{view}");
    }

    let (status, loaded) = server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let view = sessions(&server).await;
    assert_eq!(view["totals"]["sessions"], 1, "{view}");
    assert_eq!(view["sessions"][0]["session"], pinned, "{view}");
    assert_eq!(view["sessions"][0]["retained"], true, "{view}");
    assert_eq!(view["sessions"][0]["execution_current"], false, "{view}");

    let cold = chat(&server, &kept, None).await;
    assert_eq!(last_record(&server).await["session_cache"]["path"], "cold");
    assert!(cached(&cold) <= 1, "{cold}");
    let view = settled(&server, 2).await;
    assert!(
        view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["session"] == pinned && row["execution_current"] == false),
        "{view}"
    );

    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    set_context(&server, 512).await;
    let reply = turn(&server, &mut kept, "Restored again?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&reply) <= 4, "{reply}");
    let view = settled(&server, 2).await;
    for row in view["sessions"].as_array().unwrap() {
        assert_eq!(row["execution_current"], true, "{view}");
    }
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
async fn a_partly_changed_system_prompt_replaces_its_own_slot_not_another_clients() {
    let server = session_server(2, json!({"mode":"memory"})).await;
    let rules = |live: &str| format!("{}Live note: {live}", "Static household rule. ".repeat(80));
    chat(
        &server,
        &new_conversation(&rules("3 GiB free."), "Lights?"),
        None,
    )
    .await;
    let alpha = last_record(&server).await["session_cache"]["slot"].clone();
    let pirate = "Pirate storyteller who answers in rhyme. ".repeat(60);
    chat(&server, &new_conversation(&pirate, "A tale?"), None).await;
    let bravo = last_record(&server).await["session_cache"]["slot"].clone();
    assert_ne!(alpha, bravo);
    chat(
        &server,
        &new_conversation(&rules("3 GiB free."), "Fans?"),
        None,
    )
    .await;
    assert_eq!(last_record(&server).await["session_cache"]["slot"], alpha);

    let changed = chat(
        &server,
        &new_conversation(&rules("4 GiB free."), "Heating?"),
        None,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["slot"], alpha, "{record}");
    assert!(cached(&changed) > 240, "{changed}");
    let back = chat(&server, &new_conversation(&pirate, "Another?"), None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["slot"], bravo, "{record}");
    assert_eq!(record["session_cache"]["path"], "slot-hit", "{record}");
    assert!(processed(&back) <= 4, "{back}");
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
async fn sessions_can_be_retained_and_expire_only_after_their_retention() {
    let server = session_server(1, fast()).await;
    chat(&server, &conversation("rivers"), None).await;
    settled(&server, 1).await;
    let view = sessions(&server).await;
    let session = view["sessions"][0]["session"].as_str().unwrap().to_owned();
    assert_eq!(view["sessions"][0]["retained"], false);
    assert_eq!(view["sessions"][0]["expires_unix_ms"], Value::Null);
    for (body, status, code) in [
        (
            json!({"action":"retain","session":"short","retained":true}),
            StatusCode::BAD_REQUEST,
            "invalid_session",
        ),
        (
            json!({"action":"retain","session":"0".repeat(64),"retained":true}),
            StatusCode::NOT_FOUND,
            "unknown_session",
        ),
    ] {
        let (actual, error) = server.post("juiced/cache/sessions", body).await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["error"]["code"], code);
    }
    let (status, retained) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"retain","session":session,"retained":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert_eq!(retained["retained"], true);
    let generation = sessions(&server).await["configuration_generation"].clone();
    let (status, configured) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"configure","expected_generation":generation,"settings":{"retention_days":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{configured}");
    let view = sessions(&server).await;
    assert_eq!(view["settings"]["retention_days"], 1);
    assert_eq!(view["settings"]["retained"], json!([session]));
    assert_eq!(view["totals"]["sessions"], 1);
    assert_eq!(view["sessions"][0]["retained"], true);
    assert_eq!(view["sessions"][0]["expires_unix_ms"], Value::Null);
    let (status, _) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"retain","session":session,"retained":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let view = sessions(&server).await;
    assert!(view["settings"].get("retained").is_none(), "{view}");
    let expires = view["sessions"][0]["expires_unix_ms"].as_u64().unwrap();
    let used = view["sessions"][0]["last_used_unix_ms"].as_u64().unwrap();
    assert_eq!(expires - used, 86_400_000);
    let generation = view["configuration_generation"].clone();
    let (status, error) = server
        .post(
            "juiced/cache/sessions",
            json!({"action":"configure","expected_generation":generation,"settings":{"retention_days":400}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_session_settings");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(saved["session_cache"]["retention_days"], 1);
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
    server
        .wait_for(async || server.event(MODEL, "slot-save", "started"))
        .await;
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    let view = sessions(&server).await;
    assert_eq!(view["totals"]["sessions"], 1, "{view}");
    assert_eq!(view["sessions"][0]["state"], "saved", "{view}");
}

fn rules(live: &str) -> String {
    format!("{}Live note: {live}", "Static household rule. ".repeat(200))
}

const RULE_WORDS: u64 = 600;

#[tokio::test]
async fn a_message_that_changes_near_its_end_restores_the_state_that_holds_its_opening() {
    let mut server = session_server(1, fast()).await;
    let first = chat(
        &server,
        &new_conversation(&rules("3 GiB free."), "Lights?"),
        None,
    )
    .await;
    assert_eq!(cached(&first), 0);
    settled(&server, 1).await;
    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    settled(&server, 2).await;

    let changed = chat(
        &server,
        &new_conversation(&rules("4 GiB free."), "Fans?"),
        None,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&changed) >= RULE_WORDS, "{changed}");
    assert!(processed(&changed) <= 10, "{changed}");
    settled(&server, 2).await;

    let resident = chat(
        &server,
        &new_conversation(&rules("5 GiB free."), "Heating?"),
        None,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "slot-hit", "{record}");
    assert!(record["session_cache"]["restore_ms"].is_null(), "{record}");
    assert!(cached(&resident) >= RULE_WORDS, "{resident}");
    settled(&server, 2).await;

    chat(&server, &new_conversation(&pirate(), "Another tale?"), None).await;
    settled(&server, 2).await;
    server.restart().await;
    let after_restart = chat(
        &server,
        &new_conversation(&rules("6 GiB free."), "Blinds?"),
        None,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&after_restart) >= RULE_WORDS, "{after_restart}");
    settled(&server, 2).await;

    let early = |live: &str| {
        format!(
            "Live note: {live} {}",
            "Static household rule. ".repeat(200)
        )
    };
    chat(&server, &new_conversation(&early("3 GiB"), "Lights?"), None).await;
    settled(&server, 3).await;
    chat(&server, &new_conversation(&pirate(), "A third tale?"), None).await;
    settled(&server, 3).await;
    let unrelated = chat(&server, &new_conversation(&early("4 GiB"), "Lights?"), None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "cold", "{record}");
    assert!(record["session_cache"]["restore_ms"].is_null(), "{record}");
    assert!(cached(&unrelated) <= 3, "{unrelated}");
}

#[tokio::test]
async fn a_long_document_restores_for_another_question_behind_a_short_system_prompt() {
    let server = session_server(1, fast()).await;
    let document = "The meter in area nine feeds the relay. ".repeat(120);
    let ask = |question: &str| {
        new_conversation(
            "Answer from the document.",
            &format!("{document}Question: {question}"),
        )
    };
    let words = document.split_whitespace().count() as u64;
    chat(&server, &ask("Which area?"), None).await;
    settled(&server, 1).await;
    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    settled(&server, 2).await;
    let other = chat(&server, &ask("What feeds the relay?"), None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&other) >= words, "{other}");
    assert!(processed(&other) <= 8, "{other}");

    let brief = "Short note about the meter. ".repeat(20);
    assert!(brief.len() < 1024);
    let small = |question: &str| new_conversation("Answer briefly.", &format!("{brief}{question}"));
    chat(&server, &small("One?"), None).await;
    chat(&server, &new_conversation(&pirate(), "Another tale?"), None).await;
    let short = chat(&server, &small("Two?"), None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "cold", "{record}");
    assert!(cached(&short) <= 3, "{short}");
}

#[tokio::test]
async fn a_new_named_session_restores_a_named_session_that_shares_its_opening() {
    let server = session_server(1, fast()).await;
    let system = devices();
    let words = system.split_whitespace().count() as u64;
    let named = ("x-juiced-session", "thread-one");
    let mut first = new_conversation(&system, "Turn on the porch.");
    let reply = chat(&server, &first, Some(named)).await;
    first.push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
    first.push(json!({"role":"user","content":"And the hallway?"}));
    settled(&server, 1).await;
    chat(
        &server,
        &conversation("tides"),
        Some(("x-juiced-session", "other")),
    )
    .await;
    settled(&server, 2).await;

    let fresh = chat(
        &server,
        &new_conversation(&system, "Is the garage open?"),
        Some(("x-juiced-session", "thread-two")),
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["key"], "header", "{record}");
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&fresh) > words, "{fresh}");
    settled(&server, 3).await;

    chat(
        &server,
        &conversation("tides"),
        Some(("x-juiced-session", "other")),
    )
    .await;
    let own = chat(&server, &first, Some(named)).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(processed(&own) <= 6, "{own}");
    settled(&server, 3).await;

    let unnamed = chat(&server, &new_conversation(&system, "Dim the porch."), None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["key"], "prefix", "{record}");
    assert_eq!(record["session_cache"]["path"], "slot-hit", "{record}");
    assert!(cached(&unnamed) > words, "{unnamed}");
}

#[tokio::test]
async fn no_store_requests_use_saved_states_and_save_none() {
    let server = session_server(1, fast()).await;
    let system = devices();
    let words = system.split_whitespace().count() as u64;
    let no_store = Some(("x-juiced-cache", "no-store"));
    chat(
        &server,
        &new_conversation(&system, "Turn on the porch."),
        None,
    )
    .await;
    let view = settled(&server, 1).await;
    let saved = view["sessions"][0]["session"].clone();
    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    settled(&server, 2).await;

    let once = chat(
        &server,
        &new_conversation(&system, "Is the garage open?"),
        no_store,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(cached(&once) > words, "{once}");
    let view = sessions(&server).await;
    assert_eq!(model_row(&view)["slots"][0]["state"], "holding", "{view}");

    let again = chat(
        &server,
        &new_conversation(&system, "Lock the hallway."),
        no_store,
    )
    .await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "slot-hit", "{record}");
    assert_eq!(record["session_cache"]["saved_previous"], false, "{record}");
    assert!(cached(&again) > words, "{again}");
    chat(&server, &new_conversation(&pirate(), "Another tale?"), None).await;
    let view = settled(&server, 2).await;
    assert!(
        view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["session"] == saved),
        "{view}"
    );

    for values in [vec!["keep"], vec!["no-store", "no-store"]] {
        let mut request = server
            .client
            .post(format!("{}/v1/chat/completions", server.base))
            .bearer_auth(server.read_key().unwrap())
            .json(&json!({"model":MODEL,"messages":conversation("deserts")}));
        for value in values {
            request = request.header("x-juiced-cache", value);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "invalid_cache_directive", "{error}");
    }
}

#[tokio::test]
async fn a_failed_request_keeps_its_slot_and_the_record_says_what_the_engine_reused() {
    let server = session_server(3, json!({"mode":"memory"})).await;
    chat(&server, &new_conversation(&pirate(), "A tale?"), None).await;
    let taken = last_record(&server).await["session_cache"]["slot"].clone();
    let messages = new_conversation(&devices(), "Turn on the porch.");
    let failed = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .bearer_auth(server.read_key().unwrap())
        .json(&json!({"model":MODEL,"messages":messages,"fixture_status":500}))
        .send()
        .await
        .unwrap();
    assert!(!failed.status().is_success());
    let record = last_record(&server).await;
    let slot = record["session_cache"]["slot"].clone();
    assert_ne!(slot, taken, "{record}");
    assert_eq!(record["session_cache"]["path"], "cold", "{record}");

    let retried = chat(&server, &messages, None).await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["slot"], slot, "{record}");
    assert_eq!(record["session_cache"]["path"], "slot-hit", "{record}");
    assert!(processed(&retried) <= 2, "{retried}");

    // Too short for Juiced to match, yet the engine's one slot still holds a hundred tokens.
    let server = session_server(1, fast()).await;
    let letters = |tail: &str| new_conversation(&format!("{}{tail}", "k ".repeat(100)), "Go on.");
    chat(&server, &letters("one"), None).await;
    let unseen = chat(&server, &letters("two"), None).await;
    let record = last_record(&server).await;
    assert!(cached(&unseen) >= 100, "{unseen}");
    assert_eq!(record["session_cache"]["path"], "slot-hit", "{record}");
    assert!(record["session_cache"]["restore_ms"].is_null(), "{record}");
}

#[tokio::test]
async fn tune_trials_under_other_settings_leave_saved_conversations_restorable() {
    let server = Server::configured(true, |config| {
        configure(config, 1, fast());
        let shaped = std::path::Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .with_file_name("shaped.gguf");
        std::fs::write(&shaped, shaped_gguf()).unwrap();
        config["models"][0]["model_file"] = json!(shaped);
        // Tune compares the speeds an engine reports; the prompt-cache emulation reports none.
        let args = config["models"][0]["args"].as_array_mut().unwrap();
        args.retain(|argument| argument != "--emulate-prompt-cache");
        args.push(json!("--report-timings"));
        config["request_timeout_ms"] = json!(10000);
        config["resident_budget_bytes"] = json!(8u64 << 30);
    })
    .await;
    let mut a = conversation("orchards");
    let first = chat(&server, &a, None).await;
    a.push(json!({"role":"assistant","content":first["choices"][0]["message"]["content"]}));
    let view = settled(&server, 1).await;
    let saved = view["sessions"][0]["session"].clone();
    assert_eq!(view["sessions"][0]["execution_current"], true, "{view}");

    let (status, started) = server
        .post(&format!("models/{MODEL}/tune"), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let mut under_trial_settings = false;
    let mut tune = Value::Null;
    for _ in 0..6000 {
        tune = server.get(&format!("models/{MODEL}/tune")).await;
        let view = sessions(&server).await;
        let rows = view["sessions"].as_array().unwrap();
        assert!(
            rows.iter().any(|row| row["session"] == saved),
            "{tune} {view}"
        );
        under_trial_settings |= rows
            .iter()
            .any(|row| row["session"] == saved && row["execution_current"] == false);
        if tune["state"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(tune["state"], "done", "{tune}");
    assert!(
        tune["result"]["measurements"]["candidates"]
            .as_array()
            .is_some_and(|trials| trials.len() >= 2),
        "{tune}"
    );
    assert!(
        under_trial_settings,
        "no trial ran under other settings: {tune}"
    );
    assert_eq!(tune["result"]["arguments"], json!([]), "{tune}");

    turn(&server, &mut a, "After Tune?").await;
    let record = last_record(&server).await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
}
