mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{ADMIN_KEY, CLIENT_KEY, Server};

fn chat(stream: bool, text: &str) -> Value {
    json!({"model":"fixture.one","messages":[{"role":"user","content":text}],"max_tokens":4,"stream":stream})
}

async fn records(server: &Server) -> Vec<Value> {
    server.get("juiced/requests").await["data"]
        .as_array()
        .unwrap()
        .clone()
}

async fn settled(server: &Server, count: usize) -> Vec<Value> {
    server
        .wait_for(async || records(server).await.len() >= count)
        .await;
    records(server).await
}

fn last_request(health: &Value, id: &str) -> Value {
    health["protocols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|protocol| protocol["id"] == id)
        .unwrap()["last_request"]
        .clone()
}

#[tokio::test]
async fn ollama_discovery_carries_details_capabilities_and_context() {
    let server = Server::new(true).await;
    let (status, tags) = server
        .json(Method::GET, "/api/tags", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::OK, "{tags}");
    let weights = std::fs::metadata(server.directory.path().join("fixture weights.gguf")).unwrap();
    for entry in tags["models"].as_array().unwrap() {
        assert!(
            entry["modified_at"].as_str().unwrap().ends_with('Z'),
            "{entry}"
        );
        assert_eq!(entry["size"], weights.len());
        let digest = entry["digest"].as_str().unwrap();
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{entry}"
        );
        for key in ["format", "family", "parameter_size", "quantization_level"] {
            assert!(entry["details"][key].is_string(), "{key}: {entry}");
        }
        assert!(entry["details"]["families"].is_array(), "{entry}");
    }
    let (status, show) = server
        .json(
            Method::POST,
            "/api/show",
            Some(json!({"model":"fixture.one:latest"})),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{show}");
    let capabilities = show["capabilities"].as_array().unwrap();
    assert!(capabilities.contains(&json!("completion")), "{show}");
    assert!(capabilities.contains(&json!("embedding")), "{show}");
    assert!(show["model_info"]["general.architecture"].is_string());
    assert!(show["details"].is_object() && show["parameters"].is_string());
    let mut untyped = server
        .client
        .post(format!("{}/api/show", server.base))
        .body(r#"{"model":"fixture.one:latest"}"#);
    if let Some(key) = server.read_key() {
        untyped = untyped.bearer_auth(key);
    }
    let untyped = untyped.send().await.unwrap();
    assert_eq!(
        untyped.status(),
        StatusCode::OK,
        "curl -d sends no JSON type"
    );
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(chat(false, "Load it")),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, ps) = server
        .json(Method::GET, "/api/ps", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::OK, "{ps}");
    let running = ps["models"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{ps}");
    assert_eq!(running[0]["name"], "fixture.one:latest");
    assert!(running[0]["expires_at"].as_str().unwrap().ends_with('Z'));
    assert!(running[0]["size_vram"].is_u64() && running[0]["details"].is_object());
}

#[tokio::test]
async fn records_name_the_protocol_and_key_and_the_console_is_not_a_client() {
    let server = Server::new(true).await;
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(chat(false, "OpenAI client")),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = server
        .json(
            Method::POST,
            "/api/chat",
            Some(json!({"model":"fixture.one","messages":[{"role":"user","content":"Ollama client"}],"stream":false})),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let all = settled(&server, 2).await;
    assert_eq!(all[0]["protocol"], "ollama", "{all:?}");
    assert_eq!(all[1]["protocol"], "openai", "{all:?}");
    assert_eq!(all[0]["key_name"], "LEMONADE_API_KEY");
    let serialized = serde_json::to_string(&all).unwrap();
    assert!(!serialized.contains(CLIENT_KEY) && !serialized.contains("Ollama client"));
    let before = server.get("juiced/protocols/health").await;
    let openai = last_request(&before, "openai");
    assert!(openai.is_object(), "{before}");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    for (method, path, body) in [
        (Method::GET, "/v1/models", None),
        (
            Method::POST,
            "/v1/chat/completions",
            Some(chat(false, "Console test")),
        ),
    ] {
        let mut request = server
            .client
            .request(method, format!("{}{path}", server.base))
            .bearer_auth(ADMIN_KEY)
            .header("sec-fetch-site", "same-origin");
        if let Some(body) = body {
            request = request.json(&body);
        }
        assert_eq!(request.send().await.unwrap().status(), StatusCode::OK);
    }
    let all = settled(&server, 3).await;
    assert_eq!(all[0]["protocol"], "console");
    assert_eq!(all[0]["key_name"], "LEMONADE_ADMIN_API_KEY");
    let after = server.get("juiced/protocols/health").await;
    assert_eq!(last_request(&after, "openai"), openai);
}

#[tokio::test]
async fn request_history_survives_a_restart_without_content() {
    let mut server = Server::new(true).await;
    for text in ["first private prompt", "second private prompt"] {
        let (status, _) = server
            .json(
                Method::POST,
                "/v1/chat/completions",
                Some(chat(false, text)),
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let before = settled(&server, 2).await;
    server.restart().await;
    assert_eq!(records(&server).await, before);
    let file =
        std::fs::read_to_string(server.directory.path().join("request-history.json")).unwrap();
    assert!(!file.contains("private prompt") && !file.contains("fixture reply"));
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(chat(false, "third")),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let after = settled(&server, 3).await;
    assert!(after[0]["id"].as_u64().unwrap() > before[0]["id"].as_u64().unwrap());
    let (status, limited) = server
        .json(
            Method::GET,
            "/v1/juiced/requests?limit=1",
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(limited["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn opt_in_capture_is_operator_only_bounded_memory_and_cleared_when_off() {
    let mut server = Server::new(true).await;
    let set = async |server: &Server, enabled: bool, key: Option<&str>| {
        server
            .json(
                Method::POST,
                "/v1/juiced/requests/capture",
                Some(json!({"enabled": enabled})),
                key,
            )
            .await
    };
    assert_eq!(
        set(&server, true, server.read_key()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, view) = set(&server, true, server.admin_key()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["enabled"], true);
    assert_eq!(view["limits"], json!({"requests": 20, "body_bytes": 65536}));
    for stream in [false, true] {
        let response = server
            .request(
                Method::POST,
                "/v1/chat/completions",
                Some(chat(stream, "Captured prompt")),
                server.read_key(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
    }
    let all = settled(&server, 2).await;
    assert!(
        all.iter().all(|record| record["captured"] == true),
        "{all:?}"
    );
    let capture = |id: &Value| format!("/v1/juiced/requests/{id}/capture");
    let (status, streamed) = server
        .json(
            Method::GET,
            &capture(&all[0]["id"]),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{streamed}");
    assert!(
        streamed["request"]["text"]
            .as_str()
            .unwrap()
            .contains("Captured prompt")
    );
    assert_eq!(streamed["response"]["assembled"], true);
    let assembled: Value =
        serde_json::from_str(streamed["response"]["text"].as_str().unwrap()).unwrap();
    assert!(
        assembled["content"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{streamed}"
    );
    let (status, plain) = server
        .json(
            Method::GET,
            &capture(&all[1]["id"]),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        plain["response"]["text"]
            .as_str()
            .unwrap()
            .contains("Rust fixture reply")
    );
    assert_eq!(
        server
            .json(
                Method::GET,
                &capture(&all[1]["id"]),
                None,
                server.read_key()
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    server.stop();
    server.start().await;
    let file =
        std::fs::read_to_string(server.directory.path().join("request-history.json")).unwrap();
    assert!(!file.contains("Captured prompt") && !file.contains("fixture reply"));
    assert_eq!(
        server.get("juiced/requests/capture").await["enabled"],
        false
    );
    assert!(
        records(&server)
            .await
            .iter()
            .all(|record| record.get("captured").is_none())
    );
    let (status, _) = set(&server, true, server.admin_key()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(chat(false, "again")),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let latest = settled(&server, 3).await[0]["id"].clone();
    assert_eq!(
        server
            .json(Method::GET, &capture(&latest), None, server.admin_key())
            .await
            .0,
        StatusCode::OK
    );
    set(&server, false, server.admin_key()).await;
    assert_eq!(
        server
            .json(Method::GET, &capture(&latest), None, server.admin_key())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
