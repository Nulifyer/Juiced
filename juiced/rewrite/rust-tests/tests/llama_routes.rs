#![cfg(target_os = "linux")]

mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::path::PathBuf;
use support::{CLIENT_KEY, Server, voice};

fn llama_fixture(config: &mut Value, session_mode: &str) {
    config["cache_enabled"] = json!(true);
    config["session_cache"] = json!({"mode": session_mode});
    let model = &mut config["models"][0];
    model["provider"] = json!("llama-cpp");
    model["max_inflight"] = json!(1);
    model["options"] = json!({"parallel_slots": 1, "context_tokens": 512});
    model["args"].as_array_mut().unwrap().extend([
        json!("--emulate-prompt-cache"),
        json!("--slot-save-path"),
        json!("{cache}"),
    ]);
    config["models"][1]["operations"] = json!(["rerank"]);
}

async fn call(
    server: &Server,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    server.json(method, path, body, server.admin_key()).await
}

async fn complete(server: &Server, key: &str, session: &str) {
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(key)
        .header("x-juiced-session", session)
        .json(&json!({"model":"fixture.one","prompt":"Once upon a time","max_tokens":4,"stream":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
}

#[tokio::test]
async fn live_is_open_and_bare_realtime_upgrades_under_the_protocol_gate() {
    let server = Server::new(true).await;
    for method in [Method::GET, Method::HEAD] {
        let response = server
            .request(method.clone(), "/live", None, None)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method} /live");
        if method == Method::GET {
            assert_eq!(
                response.json::<Value>().await.unwrap(),
                json!({"status":"ok"})
            );
        }
    }
    let path = "/realtime?intent=transcription";
    let refused = voice::realtime(&server, path, None, None)
        .await
        .err()
        .unwrap();
    let tokio_tungstenite::tungstenite::Error::Http(response) = refused else {
        panic!("expected an HTTP refusal");
    };
    assert_eq!(response.status(), 401);
    let (socket, selected) = voice::realtime(&server, path, Some(CLIENT_KEY), Some("realtime"))
        .await
        .expect("bare /realtime upgrades with 101");
    assert_eq!(selected.as_deref(), Some("realtime"));
    drop(socket);
    let (status, body) = server
        .post(
            "juiced/protocols",
            json!({"protocol":"realtime","enabled":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refused = voice::realtime(&server, path, Some(CLIENT_KEY), None)
        .await
        .err()
        .unwrap();
    let tokio_tungstenite::tungstenite::Error::Http(response) = refused else {
        panic!("expected a disabled-protocol refusal");
    };
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn tokenize_slots_and_named_slot_files_follow_the_cache_owner() {
    let server = Server::configured(true, |config| llama_fixture(config, "off")).await;
    let admin = server.admin_key().unwrap();

    let (status, error) = call(&server, Method::GET, "/v1/slots", None).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("model_not_loaded"))
    );
    let (status, error) = call(&server, Method::POST, "/v1/tokenize", Some(json!({}))).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("content_required"))
    );
    let (status, error) = call(
        &server,
        Method::POST,
        "/v1/tokenize",
        Some(json!({"content":"a"})),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("model_not_loaded"))
    );

    complete(&server, admin, "story.bin").await;

    let (status, slots) = server
        .json(Method::GET, "/v1/slots", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(status, StatusCode::OK, "{slots}");
    assert_eq!(
        slots,
        json!([{"id":0,"n_ctx":512,"is_processing":false,"state":"idle"}])
    );
    let (status, tokens) = server
        .json(Method::POST, "/api/v1/tokenize", Some(json!({"model":"fixture.one","content":"one two three","with_pieces":true,"extra":1})), Some(CLIENT_KEY))
        .await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["tokens"].as_array().unwrap().len(), 3);
    assert_eq!(tokens["tokens"][1]["piece"], "two");
    assert_eq!(
        tokens["received"],
        json!({"content":"one two three","with_pieces":true})
    );
    let (status, text) = call(
        &server,
        Method::POST,
        "/v0/detokenize",
        Some(json!({"tokens":[1,2]})),
    )
    .await;
    assert_eq!(
        (status, text["content"].as_str()),
        (StatusCode::OK, Some("word word"))
    );
    let (status, error) = call(
        &server,
        Method::POST,
        "/v1/detokenize",
        Some(json!({"tokens":["x"]})),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("tokens_required"))
    );
    let (status, error) = call(
        &server,
        Method::POST,
        "/v1/tokenize",
        Some(json!({"model":"fixture.two","content":"a"})),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("unsupported_operation"))
    );

    for (path, code) in [
        ("/v1/slots/0", "action_required"),
        ("/v1/slots/0?action=bogus", "invalid_action"),
        ("/v1/slots/x?action=erase", "invalid_slot"),
        ("/v1/slots/3?action=erase", "invalid_slot"),
        ("/v1/slots/0?action=save", "filename_required"),
    ] {
        let (status, error) = call(&server, Method::POST, path, Some(json!({}))).await;
        assert_eq!(
            (status, error["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code)),
            "{path}"
        );
    }

    let save = json!({"filename":"story.bin"});
    let (status, saved) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=save",
        Some(save.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["id_slot"], 0);
    assert_eq!(saved["filename"], "story.bin");
    assert!(saved["n_saved"].as_u64().unwrap() > 0);
    let first = saved["juiced"]["snapshot"].clone();
    complete(&server, admin, "story.bin").await;
    let (status, again) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=save",
        Some(save.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_ne!(again["juiced"]["snapshot"], first);
    server
        .wait_for(async || {
            server.get("juiced/cache").await["data"]
                .as_array()
                .unwrap()
                .len()
                == 1
        })
        .await;
    assert_eq!(
        server.get("juiced/cache").await["data"][0]["id"],
        again["juiced"]["snapshot"]
    );

    complete(&server, admin, "another.bin").await;
    let (status, error) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=save",
        Some(save.clone()),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("cache_context_mismatch"))
    );

    let (status, restored) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=restore",
        Some(save.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["filename"], "story.bin");
    assert!(restored["n_restored"].as_u64().unwrap() > 0);
    let (status, error) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=restore",
        Some(json!({"filename":"missing.bin"})),
    )
    .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("slot_file_not_found"))
    );
    let (status, error) = server
        .json(
            Method::POST,
            "/v1/slots/0?action=restore",
            Some(save.clone()),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("slot_file_not_found")),
        "another credential cannot restore this file"
    );

    let (status, erased) = server
        .json(
            Method::POST,
            "/v1/slots/0?action=erase",
            None,
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{erased}");
    assert_eq!(erased["id_slot"], 0);
    assert!(erased["n_erased"].as_u64().unwrap() > 0);
    assert!(server.event("fixture.one", "slot-erase", "completed"));
    let (status, error) = call(&server, Method::POST, "/v1/slots/0?action=save", Some(save)).await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("cache_context_mismatch"))
    );
}

#[tokio::test]
async fn persistent_session_snapshots_keep_slot_ownership() {
    let server = Server::configured(true, |config| llama_fixture(config, "persistent")).await;
    complete(&server, server.admin_key().unwrap(), "story.bin").await;
    for action in ["save", "restore", "erase"] {
        let (status, error) = call(
            &server,
            Method::POST,
            &format!("/v1/slots/0?action={action}"),
            Some(json!({"filename":"story.bin"})),
        )
        .await;
        assert_eq!(
            (status, error["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("cache_sessions_managed")),
            "{action}"
        );
    }
}

#[tokio::test]
async fn rerank_stats_and_voice_routes_answer_with_native_shapes() {
    let server = Server::configured(true, |config| llama_fixture(config, "off")).await;
    let body = json!({"model":"fixture.two","query":"capital of france","documents":["paris is the capital of france","berlin"]});
    for path in ["/v1/rerank", "/api/v1/reranking", "/v0/reranker"] {
        let (status, reply) = server
            .json(Method::POST, path, Some(body.clone()), Some(CLIENT_KEY))
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {reply}");
        assert_eq!(reply["object"], "list");
        assert_eq!(reply["results"][0]["index"], 0);
        assert!(
            reply["results"][0]["relevance_score"].as_f64().unwrap()
                > reply["results"][1]["relevance_score"].as_f64().unwrap()
        );
    }
    let (status, error) = server
        .json(
            Method::POST,
            "/v1/rerank",
            Some(json!({"model":"fixture.two","documents":[]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_request"))
    );
    let (status, error) = server
        .json(
            Method::POST,
            "/v1/rerank",
            Some(json!({"model":"fixture.one","query":"q","documents":[]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("unsupported_operation"))
    );
    assert_eq!(
        server
            .request(Method::POST, "/v1/rerank", Some(body), None)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    complete(&server, CLIENT_KEY, "stats").await;
    let (status, stats) = server
        .json(Method::GET, "/v1/stats", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert!(stats["request_count_total"].as_u64().unwrap() >= 1);
    assert!(stats["output_tokens_total"].as_u64().unwrap() > 0);
    for field in [
        "time_to_first_token",
        "tokens_per_second",
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "cache_tokens",
        "routing_decisions_total",
        "routing_switches_total",
    ] {
        assert!(stats.get(field).is_some(), "{field}");
    }
    assert_eq!(stats["live"]["active_requests"], 0);
    assert_eq!(stats["last_request"]["model"], "fixture.one");

    let (status, error) = server
        .json(Method::GET, "/v1/audio/voices", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("model_required"))
    );
    let (status, error) = server
        .json(
            Method::GET,
            "/v1/audio/voices?model=fixture.one",
            None,
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("voices_unsupported"))
    );
    let (status, error) = server
        .json(
            Method::GET,
            "/v1/audio/voices?model=missing",
            None,
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_model"))
    );
}

fn real_assets() -> (PathBuf, PathBuf) {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    assert!(engine.is_file() && model.is_file());
    (engine, model)
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; real llama-server passthroughs"]
async fn real_llama_server_tokenize_slots_and_rerank() {
    let (engine, model) = real_assets();
    let args = |extra: &[&str]| {
        let mut args = vec![
            "--host",
            "127.0.0.1",
            "--port",
            "{port}",
            "--model",
            "{model}",
            "--ctx-size",
            "256",
            "--parallel",
            "1",
            "--threads",
            "2",
            "--n-gpu-layers",
            "0",
        ];
        args.extend_from_slice(extra);
        json!(args)
    };
    let server = Server::configured(true, |config| {
        config["cache_enabled"] = json!(true);
        config["resident_budget_bytes"] = json!(2048u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([
            {"id":"stories","model_file":model,"command":engine,"provider":"llama-cpp","args":args(&["--slot-save-path","{cache}"]),
             "resident_bytes":512u64*1024*1024,"max_inflight":1,"operations":["completions"],"policy":"on-demand"},
            {"id":"stories-rerank","model_file":model,"command":engine,"provider":"llama-cpp","args":args(&["--reranking"]),
             "resident_bytes":512u64*1024*1024,"max_inflight":1,"operations":["rerank"],"policy":"on-demand"},
        ]);
    })
    .await;
    let admin = server.admin_key().unwrap();
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(admin)
        .header("x-juiced-session", "tale.bin")
        .json(&json!({"model":"stories","prompt":"Once upon a time","max_tokens":8,"temperature":0,"cache_prompt":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;

    let (status, tokens) = call(
        &server,
        Method::POST,
        "/v1/tokenize",
        Some(json!({"content":"Once upon a time","with_pieces":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    let ids: Vec<Value> = tokens["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|token| token["id"].clone())
        .collect();
    assert_eq!(ids.len(), 4);
    let (status, text) = call(
        &server,
        Method::POST,
        "/v1/detokenize",
        Some(json!({"tokens":ids})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(text["content"].as_str().unwrap().trim(), "Once upon a time");

    let (status, slots) = call(&server, Method::GET, "/v1/slots", None).await;
    assert_eq!(status, StatusCode::OK, "{slots}");
    assert_eq!(slots[0]["id"], 0);
    assert_eq!(slots[0]["state"], "idle");
    assert!(slots[0].get("params").is_none() && slots[0].get("prompt").is_none());

    let file = json!({"filename":"tale.bin"});
    let (status, saved) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=save",
        Some(file.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["n_saved"].as_u64().unwrap() > 0);
    let (status, erased) = call(&server, Method::POST, "/v1/slots/0?action=erase", None).await;
    assert_eq!(status, StatusCode::OK, "{erased}");
    assert_eq!(erased["n_erased"], saved["n_saved"]);
    let (status, restored) = call(
        &server,
        Method::POST,
        "/v1/slots/0?action=restore",
        Some(file),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["n_restored"], saved["n_saved"]);
    println!("real-slots-receipt saved={saved} erased={erased} restored={restored}");

    let (status, ranked) = call(&server, Method::POST, "/v1/rerank", Some(json!({"model":"stories-rerank","query":"a princess","documents":["Once upon a time there was a princess.","The cat sat."]}))).await;
    assert_eq!(status, StatusCode::OK, "{ranked}");
    assert_eq!(ranked["object"], "list");
    assert_eq!(ranked["results"].as_array().unwrap().len(), 2);
    assert!(ranked["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
    println!("real-rerank-receipt {ranked}");
}
