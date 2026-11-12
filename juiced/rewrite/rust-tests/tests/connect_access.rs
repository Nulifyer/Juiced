mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{ADMIN_KEY, CLIENT_KEY, Server};

async fn create(server: &Server, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    server
        .json(Method::POST, "/v1/juiced/access/keys", Some(body), key)
        .await
}

async fn status(server: &Server, method: Method, path: &str, key: Option<&str>) -> StatusCode {
    let body = (method == Method::POST)
        .then(|| json!({"model": "fixture.one", "messages": [{"role": "user", "content": "hi"}]}));
    server
        .request(method, path, body, key)
        .await
        .unwrap()
        .status()
}

async fn code(server: &Server, method: Method, path: &str, key: Option<&str>) -> Value {
    let body = (method == Method::POST).then(|| json!({"model": "fixture.one", "input": "hi"}));
    let (_, value) = server.json(method, path, body, key).await;
    value["error"]["code"].clone()
}

fn secret(created: &Value) -> String {
    created["created"]["secret"].as_str().unwrap().to_owned()
}

fn issued<'a>(view: &'a Value, name: &str) -> &'a Value {
    view["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["name"] == name)
        .unwrap_or_else(|| panic!("key {name} listed"))
}

#[tokio::test]
async fn issued_keys_enforce_scopes_persist_hashes_and_expire() {
    let mut server = Server::new(false).await;
    let (_, view) = server
        .json(Method::GET, "/v1/juiced/access", None, None)
        .await;
    assert_eq!(view["mode"], "trusted-lan");
    assert_eq!(view["keys"], json!([]));
    for body in [
        json!({"name": "client", "scope": "inference"}),
        json!({"name": "console", "scope": "operator", "expires_in_days": 7}),
    ] {
        let (status, error) = create(&server, None, body).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "operator_key_first");
    }
    let (created, console) = create(
        &server,
        None,
        json!({"name": "console", "scope": "operator"}),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);
    assert_eq!(console["mode"], "keys");
    let operator = secret(&console);
    assert!(operator.starts_with("juiced_"));
    assert_eq!(
        console["created"]["key"]["prefix"],
        operator[..13].to_owned()
    );
    assert_eq!(
        status(&server, Method::GET, "/v1/models", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(&server, Method::GET, "/v1/juiced/access", Some(&operator)).await,
        StatusCode::OK
    );
    let (_, client) = create(
        &server,
        Some(&operator),
        json!({"name": "client", "scope": "inference", "expires_in_days": 30}),
    )
    .await;
    let client = secret(&client);
    let (_, reader) = create(
        &server,
        Some(&operator),
        json!({"name": "reader", "scope": "read"}),
    )
    .await;
    let reader = secret(&reader);
    let (status_code, error) = create(
        &server,
        Some(&client),
        json!({"name": "escalate", "scope": "operator"}),
    )
    .await;
    assert_eq!(status_code, StatusCode::UNAUTHORIZED);
    assert_eq!(error["error"]["code"], "operator_required");

    assert_eq!(
        status(&server, Method::GET, "/v1/models", Some(&reader)).await,
        StatusCode::OK
    );
    assert_eq!(
        code(&server, Method::POST, "/v1/embeddings", Some(&reader)).await,
        "inference_required"
    );
    assert_eq!(
        code(&server, Method::GET, "/v1/juiced/access", Some(&client)).await,
        "operator_required"
    );
    assert_eq!(
        status(&server, Method::POST, "/v1/chat/completions", Some(&client)).await,
        StatusCode::OK
    );
    let header_key = server
        .client
        .get(format!("{}/api/tags", server.base))
        .header("x-api-key", &reader)
        .send()
        .await
        .unwrap();
    assert_eq!(header_key.status(), StatusCode::OK);

    let (_, listed) = server
        .json(Method::GET, "/v1/juiced/access", None, Some(&operator))
        .await;
    let text = listed.to_string();
    for secret in [&operator, &client, &reader] {
        assert!(!text.contains(secret.as_str()), "listing leaks a secret");
    }
    assert_eq!(issued(&listed, "client")["scope"], "inference");
    assert!(issued(&listed, "client")["expires_at_ms"].is_u64());
    assert!(issued(&listed, "client")["last_used_at_ms"].is_u64());
    assert_eq!(issued(&listed, "console")["source"], "issued");
    let path = server.directory.path().join("api-keys.json");
    let stored = std::fs::read_to_string(&path).unwrap();
    for secret in [&operator, &client, &reader] {
        assert!(!stored.contains(secret.as_str()), "store keeps a secret");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    let id = |name: &str| issued(&listed, name)["id"].as_str().unwrap().to_owned();
    let (status_code, error) = server
        .json(
            Method::POST,
            "/v1/juiced/access/keys/revoke",
            Some(json!({"id": id("console")})),
            Some(&operator),
        )
        .await;
    assert_eq!(status_code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "last_operator_key");
    let (status_code, after) = server
        .json(
            Method::POST,
            "/v1/juiced/access/keys/revoke",
            Some(json!({"id": id("client")})),
            Some(&operator),
        )
        .await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(after["keys"].as_array().unwrap().len(), 2);
    assert_eq!(
        status(&server, Method::GET, "/v1/models", Some(&client)).await,
        StatusCode::UNAUTHORIZED
    );

    server.stop();
    let mut image: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let reader_entry = image["keys"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|key| key["name"] == "reader")
        .unwrap();
    assert!(reader_entry["last_used_at_ms"].is_u64());
    reader_entry["expires_at_ms"] = json!(1);
    std::fs::write(&path, image.to_string()).unwrap();
    server.start().await;
    assert_eq!(
        status(&server, Method::GET, "/v1/juiced/access", Some(&operator)).await,
        StatusCode::OK
    );
    assert_eq!(
        code(&server, Method::GET, "/v1/models", Some(&reader)).await,
        "api_key_expired"
    );
    let (_, listed) = server
        .json(Method::GET, "/v1/juiced/access", None, Some(&operator))
        .await;
    assert_eq!(issued(&listed, "reader")["expired"], true);
    for name in ["reader", "console"] {
        let (status_code, _) = server
            .json(
                Method::POST,
                "/v1/juiced/access/keys/revoke",
                Some(json!({"id": id(name)})),
                Some(&operator),
            )
            .await;
        assert_eq!(status_code, StatusCode::OK, "revoke {name}");
    }
    let (_, view) = server
        .json(Method::GET, "/v1/juiced/access", None, None)
        .await;
    assert_eq!(view["mode"], "trusted-lan");

    server.stop();
    std::fs::write(&path, b"{damaged").unwrap();
    server.start().await;
    let (status_code, error) = server.json(Method::GET, "/v1/models", None, None).await;
    assert_eq!(status_code, StatusCode::UNAUTHORIZED);
    assert_eq!(error["error"]["code"], "unauthorized");
    assert_eq!(
        code(&server, Method::GET, "/v1/models", Some("juiced_any")).await,
        "key_store_unavailable"
    );
}

#[tokio::test]
async fn environment_keys_stay_valid_read_only_and_anchor_operators() {
    let server = Server::new(true).await;
    let (_, view) = server
        .json(Method::GET, "/v1/juiced/access", None, Some(ADMIN_KEY))
        .await;
    assert_eq!(view["mode"], "keys");
    assert_eq!(
        view["keys"],
        json!([
            {"id": "env:LEMONADE_ADMIN_API_KEY", "name": "LEMONADE_ADMIN_API_KEY", "scope": "operator", "source": "environment"},
            {"id": "env:LEMONADE_API_KEY", "name": "LEMONADE_API_KEY", "scope": "inference", "source": "environment"},
        ])
    );
    let (created, body) = create(
        &server,
        Some(ADMIN_KEY),
        json!({"name": "pi", "scope": "inference", "expires_in_days": 1}),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);
    let issued = secret(&body);
    for key in [CLIENT_KEY, ADMIN_KEY, issued.as_str()] {
        assert_eq!(
            status(&server, Method::GET, "/v1/models", Some(key)).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        code(&server, Method::GET, "/v1/juiced/access", Some(CLIENT_KEY)).await,
        "operator_required"
    );
    let (status_code, error) = server
        .json(
            Method::POST,
            "/v1/juiced/access/keys/revoke",
            Some(json!({"id": "env:LEMONADE_API_KEY"})),
            Some(ADMIN_KEY),
        )
        .await;
    assert_eq!(status_code, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "api_key_not_found");
}

async fn toggle(server: &Server, protocol: &str, enabled: bool) {
    let (status_code, body) = server
        .post(
            "juiced/protocols",
            json!({"protocol": protocol, "enabled": enabled}),
        )
        .await;
    assert_eq!(status_code, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"protocol": protocol, "enabled": enabled}));
}

#[tokio::test]
async fn disabled_protocols_reject_routes_and_leave_discovery() {
    let mut server = Server::new(true).await;
    toggle(&server, "ollama", false).await;
    toggle(&server, "openai", false).await;
    let (status_code, error) = server
        .json(Method::GET, "/api/tags", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(status_code, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "protocol_disabled");
    assert!(error["error"].as_str().unwrap().contains("Ollama"));
    assert_eq!(
        status(&server, Method::GET, "/api/tags", None).await,
        StatusCode::UNAUTHORIZED
    );
    for (method, path) in [
        (Method::POST, "/v1/chat/completions"),
        (Method::GET, "/v1/models"),
        (Method::POST, "/api/v1/embeddings"),
    ] {
        assert_eq!(
            code(&server, method, path, Some(CLIENT_KEY)).await,
            "protocol_disabled",
            "{path}"
        );
    }
    for path in [
        "/api/v1/health",
        "/v1/juiced/catalog",
        "/v1/models/fixture.one",
    ] {
        assert_eq!(
            status(&server, Method::GET, path, Some(CLIENT_KEY)).await,
            StatusCode::OK,
            "{path}"
        );
    }
    let endpoints = server.get("juiced/endpoints").await;
    let paths: Vec<&str> = endpoints["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert!(!paths.iter().any(|path| path.starts_with("/api/")));
    assert!(!paths.contains(&"/chat/completions") && !paths.contains(&"/models"));
    assert!(paths.contains(&"/v1/messages") && paths.contains(&"/juiced/catalog"));
    let protocols = server.get("juiced/protocols").await;
    let ids: Vec<&str> = protocols["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["anthropic", "mcp", "automatic1111", "realtime", "wyoming"]
    );
    let config: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(
        config["protocols"],
        json!({"disabled": ["openai", "ollama"]})
    );

    server.restart().await;
    let (_, error) = server
        .json(Method::GET, "/api/tags", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(error["code"], "protocol_disabled");
    toggle(&server, "ollama", true).await;
    toggle(&server, "openai", true).await;
    toggle(&server, "openai", true).await;
    for path in ["/api/tags", "/v1/models"] {
        assert_eq!(
            status(&server, Method::GET, path, Some(CLIENT_KEY)).await,
            StatusCode::OK
        );
    }
    let config: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert!(config.get("protocols").is_none());
    let (status_code, error) = server
        .post(
            "juiced/protocols",
            json!({"protocol": "gopher", "enabled": false}),
        )
        .await;
    assert_eq!(status_code, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "unknown_protocol");
}

fn entry<'a>(health: &'a Value, id: &str) -> &'a Value {
    health["protocols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn protocol_health_checks_routes_models_and_last_requests() {
    let server = Server::configured(true, |config| {
        config["wyoming"] = json!({"listen": "127.0.0.1:0"});
    })
    .await;
    assert_eq!(
        code(
            &server,
            Method::GET,
            "/v1/juiced/protocols/health",
            Some(CLIENT_KEY)
        )
        .await,
        "operator_required"
    );
    let health = server.get("juiced/protocols/health").await;
    assert_eq!(health["schema_version"], 1);
    let ids: Vec<&str> = health["protocols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "openai",
            "anthropic",
            "ollama",
            "mcp",
            "automatic1111",
            "realtime",
            "wyoming"
        ]
    );
    for id in [
        "openai",
        "anthropic",
        "ollama",
        "mcp",
        "automatic1111",
        "realtime",
    ] {
        assert_eq!(entry(&health, id)["check"]["ok"], true, "{id}: {health}");
        assert_eq!(entry(&health, id)["last_request"], Value::Null, "{id}");
    }
    assert_eq!(entry(&health, "openai")["status"], "idle");
    assert_eq!(entry(&health, "openai")["models"], 2);
    assert_eq!(entry(&health, "automatic1111")["status"], "no_models");
    assert_eq!(entry(&health, "realtime")["status"], "no_models");
    assert_eq!(entry(&health, "wyoming")["status"], "disabled");
    assert_eq!(entry(&health, "wyoming")["check"], Value::Null);
    assert_eq!(
        server.get("juiced/requests").await["data"],
        json!([]),
        "probes must not start inference"
    );

    assert_eq!(
        status(&server, Method::GET, "/v1/models", Some(CLIENT_KEY)).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&server, Method::GET, "/api/tags", Some("wrong")).await,
        StatusCode::UNAUTHORIZED
    );
    toggle(&server, "mcp", false).await;
    toggle(&server, "wyoming", true).await;
    let health = server.get("juiced/protocols/health").await;
    assert_eq!(entry(&health, "openai")["status"], "healthy");
    assert_eq!(entry(&health, "openai")["last_request"]["status"], 200);
    assert_eq!(entry(&health, "ollama")["status"], "idle");
    assert_eq!(entry(&health, "ollama")["last_request"]["status"], 401);
    assert_eq!(entry(&health, "mcp")["status"], "disabled");
    assert_eq!(entry(&health, "mcp")["enabled"], false);
    assert_eq!(entry(&health, "mcp")["check"], Value::Null);
    let wyoming = entry(&health, "wyoming");
    assert_eq!(wyoming["enabled"], true);
    assert!(wyoming["address"].as_str().is_some(), "{wyoming}");
    assert_eq!(wyoming["check"]["ok"], true);
    assert_eq!(
        server.get("juiced/wyoming").await["settings"]["enabled"],
        true
    );
    toggle(&server, "wyoming", false).await;
    assert_eq!(
        server.get("juiced/wyoming").await["settings"]["enabled"],
        false
    );
}
