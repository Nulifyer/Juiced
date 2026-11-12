mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{COMPAT_ROUTES, Server};

async fn configured(extra: &[&str]) -> Server {
    Server::configured(true, |config| {
        for model in config["models"].as_array_mut().unwrap() {
            model["args"]
                .as_array_mut()
                .unwrap()
                .extend(extra.iter().map(|value| json!(value)));
        }
    })
    .await
}
async fn post(server: &Server, path: &str, body: Value) -> (StatusCode, Value) {
    server
        .json(Method::POST, path, Some(body), server.read_key())
        .await
}
fn anthropic(stream: bool) -> Value {
    json!({"model":"fixture.one","messages":[{"role":"user","content":"Pipeline fixture"}],"max_tokens":4,"stream":stream})
}

#[tokio::test]
async fn every_fixed_protocol_route_enforces_auth_and_read_routes_have_no_loading_side_effects() {
    let server = Server::new(true).await;
    for (method, path, _) in COMPAT_ROUTES {
        let method: Method = method.parse().unwrap();
        for key in [None, Some("invalid-key")] {
            assert_eq!(
                server
                    .request(method.clone(), path, Some(json!({})), key)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }
    for path in ["/api/tags", "/api/ps", "/api/version"] {
        let (status, value) = server
            .json(Method::GET, path, None, server.read_key())
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        if path == "/api/tags" {
            assert_eq!(value["models"].as_array().unwrap().len(), 2);
        }
        if path == "/api/ps" {
            assert!(value["models"].as_array().unwrap().is_empty());
        }
    }
    let (status, show) = post(&server, "/api/show", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK, "{show}");
    let (status, _) = post(&server, "/api/show", json!({"model":"missing"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn anthropic_and_ollama_nonstream_and_native_embeddings_have_real_worker_data() {
    let server = Server::new(true).await;
    let (status, message) = post(&server, "/v1/messages", anthropic(false)).await;
    assert_eq!(status, StatusCode::OK, "{message}");
    assert_eq!(message["type"], "message");
    assert_eq!(message["stop_reason"], "end_turn");
    assert!(!message["content"].as_array().unwrap().is_empty());
    let mut count = anthropic(false);
    count.as_object_mut().unwrap().remove("max_tokens");
    let (status, tokens) = post(&server, "/v1/messages/count_tokens", count).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["input_tokens"], 7);
    for (path, body) in [
        (
            "/api/chat",
            json!({"model":"fixture.one","messages":[{"role":"user","content":"Pipeline"}],"stream":false}),
        ),
        (
            "/api/generate",
            json!({"model":"fixture.one","prompt":"Pipeline","stream":false}),
        ),
    ] {
        let (status, output) = post(&server, path, body).await;
        assert_eq!(status, StatusCode::OK, "{output}");
        assert_eq!(output["done"], true);
        assert_eq!(output["model"], "fixture.one");
        assert_eq!(output["done_reason"], "stop");
    }
    for (path, body, field) in [
        (
            "/api/embed",
            json!({"model":"fixture.one","input":"Pipeline"}),
            "embeddings",
        ),
        (
            "/api/embeddings",
            json!({"model":"fixture.one","prompt":"Pipeline"}),
            "embedding",
        ),
    ] {
        let (status, output) = post(&server, path, body).await;
        assert_eq!(status, StatusCode::OK, "{output}");
        assert!(!output[field].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
async fn anthropic_sse_and_ollama_ndjson_reach_finished_terminal_events() {
    let server = configured(&["--finite-stream"]).await;
    for (path, body) in [
        ("/v1/messages", anthropic(true)),
        (
            "/api/chat",
            json!({"model":"fixture.one","messages":[{"role":"user","content":"Pipeline"}],"stream":true}),
        ),
        (
            "/api/generate",
            json!({"model":"fixture.one","prompt":"Pipeline","stream":true}),
        ),
    ] {
        let response = server
            .request(Method::POST, path, Some(body), server.read_key())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers()["content-type"]
            .to_str()
            .unwrap()
            .to_owned();
        let text = response.text().await.unwrap();
        assert!(text.contains('λ'), "{path} stream: {text}");
        if path == "/v1/messages" {
            assert!(content_type.contains("text/event-stream"));
            assert!(text.contains("event: message_start"));
            assert!(text.contains("event: message_stop"));
            assert!(text.contains("\"stop_reason\":\"end_turn\""));
        } else {
            assert!(content_type.contains("ndjson"));
            let rows: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert!(rows.len() >= 3);
            assert_eq!(rows.last().unwrap()["done"], true);
            assert_eq!(rows.last().unwrap()["done_reason"], "stop");
        }
    }
}

#[tokio::test]
async fn compatibility_failures_use_protocol_shapes_and_never_start_workers() {
    let server = Server::new(true).await;
    for path in [
        "/v1/messages",
        "/v1/messages/count_tokens",
        "/api/chat",
        "/api/generate",
        "/api/embed",
        "/api/embeddings",
        "/api/show",
    ] {
        let (status, error) = post(&server, path, json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {error}");
        assert!(error.get("error").is_some());
        let response = server
            .client
            .post(format!("{}{path}", server.base))
            .bearer_auth(server.read_key().unwrap())
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response
                .json::<Value>()
                .await
                .unwrap()
                .get("error")
                .is_some()
        );
    }
    let (status, error) = post(
        &server,
        "/v1/messages",
        json!({"model":"fixture.one","messages":[],"max_tokens":4}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["type"], "error");
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn mcp_initialize_list_tools_and_all_inference_tools_use_actual_worker_pipelines() {
    let server = Server::new(true).await;
    for (method, params) in [
        ("initialize", json!({})),
        ("tools/list", json!({})),
        (
            "tools/call",
            json!({"name":"lemonade_list_models","arguments":{}}),
        ),
        (
            "tools/call",
            json!({"name":"lemonade_chat","arguments":{"model":"fixture.one","messages":[{"role":"user","content":"Pipeline"}]}}),
        ),
        (
            "tools/call",
            json!({"name":"lemonade_complete","arguments":{"model":"fixture.one","prompt":"Pipeline"}}),
        ),
        (
            "tools/call",
            json!({"name":"lemonade_embed","arguments":{"model":"fixture.one","input":"Pipeline"}}),
        ),
    ] {
        let (status, value) = post(
            &server,
            "/mcp",
            json!({"jsonrpc":"2.0","id":"pipeline","method":method,"params":params}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], "pipeline");
        assert!(value.get("error").is_none(), "{value}");
        assert_ne!(value["result"]["isError"], true, "{value}");
        if method == "initialize" {
            assert_eq!(value["result"]["protocolVersion"], "2025-06-18");
        } else if method == "tools/list" {
            assert_eq!(value["result"]["tools"].as_array().unwrap().len(), 4);
        } else if params["name"] == "lemonade_list_models" {
            assert_eq!(
                value["result"]["structuredContent"]["models"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        } else if params["name"] == "lemonade_embed" {
            assert_eq!(
                value["result"]["structuredContent"]["data"][0]["embedding"],
                json!([0.25, -0.5])
            );
        } else {
            let worker = &value["result"]["structuredContent"];
            assert_eq!(worker["model"], "fixture.one");
            assert_eq!(worker["received_body_sha256"].as_str().unwrap().len(), 64);
            assert_eq!(worker["received_authorization"], false);
        }
    }
    let response = server
        .request(
            Method::POST,
            "/mcp",
            Some(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            server.read_key(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let (status, batch) = post(&server, "/mcp", json!([])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(batch["error"]["code"], -32600);
    for (header, value, status) in [
        ("origin", "https://example.invalid", StatusCode::FORBIDDEN),
        (
            "mcp-protocol-version",
            "2024-11-05",
            StatusCode::BAD_REQUEST,
        ),
        ("accept", "text/event-stream", StatusCode::NOT_ACCEPTABLE),
    ] {
        let response = server
            .client
            .post(format!("{}/mcp", server.base))
            .bearer_auth(server.read_key().unwrap())
            .header(header, value)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert!(
            response
                .json::<Value>()
                .await
                .unwrap()
                .get("error")
                .is_some()
        );
    }
}

#[tokio::test]
async fn mcp_cancel_notification_aborts_upstream_without_reloading_worker() {
    let server = configured(&["--inference-delay-ms", "2000"]).await;
    let client = server.client.clone();
    let url = format!("{}/mcp", server.base);
    let key = server.read_key().unwrap();
    let mut pending = tokio::spawn(async move {
        client.post(url).bearer_auth(key).header("x-juiced-session","cancel-session").json(&json!({"jsonrpc":"2.0","id":"cancel-me","method":"tools/call","params":{"name":"lemonade_chat","arguments":{"model":"fixture.one","messages":[{"role":"user","content":"Pipeline"}]}}})).send().await.unwrap()
    });
    server
        .wait_for(async || server.event("fixture.one", "request", "started"))
        .await;
    let pid = server.model("fixture.one").await["pid"].clone();
    for (token, session) in [
        (server.admin_key().unwrap(), "cancel-session"),
        (key, "different-session"),
    ] {
        let response=server.client.post(format!("{}/mcp",server.base)).bearer_auth(token).header("x-juiced-session",session).json(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"cancel-me"}})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut pending)
                .await
                .is_err()
        );
        assert!(!server.event("fixture.one", "request", "cancelled"));
    }

    let response=server.client.post(format!("{}/mcp",server.base)).bearer_auth(key).header("x-juiced-session","cancel-session").json(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"cancel-me"}})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    assert!(cancelled.bytes().await.unwrap().is_empty());
    server
        .wait_for(async || {
            server.event("fixture.one", "request", "cancelled")
                && server.get("juiced/state").await["resources"]["active_requests"] == 0
        })
        .await;
    assert_eq!(server.model("fixture.one").await["pid"], pid);
}
