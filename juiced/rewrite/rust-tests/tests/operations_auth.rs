mod support;

use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader},
    process::Stdio,
    time::Duration,
};
use support::{ADMIN_KEY, CLIENT_KEY, Server, binary};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(
    server: &Server,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = server
        .client
        .request(method, format!("{}{path}", server.base));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.unwrap()
}

fn header<'a>(response: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap())
}

#[tokio::test]
async fn origins_follow_the_native_policy_and_trusted_preflights_list_authorization() {
    let server = Server::new(true).await;
    let bearer = format!("Bearer {CLIENT_KEY}");
    let foreign = send(
        &server,
        Method::GET,
        "/v1/health",
        &[
            ("origin", "https://evil.example"),
            ("authorization", &bearer),
        ],
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    assert_eq!(header(&foreign, "vary"), Some("Origin"));
    assert_eq!(header(&foreign, "access-control-allow-origin"), None);
    assert_eq!(
        foreign.json::<Value>().await.unwrap()["error"]["code"],
        "origin_not_allowed"
    );
    let refused = send(
        &server,
        Method::OPTIONS,
        "/v1/chat/completions",
        &[
            ("origin", "https://evil.example"),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "authorization"),
        ],
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let preflight = send(
        &server,
        Method::OPTIONS,
        "/v1/chat/completions",
        &[
            ("origin", "http://localhost:5173"),
            ("access-control-request-method", "POST"),
            (
                "access-control-request-headers",
                "authorization, content-type, x-stainless-os",
            ),
            ("access-control-request-private-network", "true"),
        ],
    )
    .await;
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        header(&preflight, "access-control-allow-origin"),
        Some("http://localhost:5173")
    );
    assert_eq!(
        header(&preflight, "access-control-allow-private-network"),
        Some("true")
    );
    let allowed: BTreeSet<String> = header(&preflight, "access-control-allow-headers")
        .unwrap()
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .collect();
    for name in [
        "authorization",
        "content-type",
        "x-api-key",
        "x-stainless-os",
    ] {
        assert!(allowed.contains(name), "{name} missing from {allowed:?}");
    }
    assert!(
        header(&preflight, "access-control-allow-methods")
            .unwrap()
            .contains("POST")
    );
    let unauthenticated = send(
        &server,
        Method::GET,
        "/v1/models",
        &[("origin", "http://127.0.0.1:3000")],
    )
    .await;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header(&unauthenticated, "access-control-allow-origin"),
        Some("http://127.0.0.1:3000")
    );
    for (origin, host, status) in [
        (
            "https://juiced.example.net",
            "juiced.example.net",
            StatusCode::OK,
        ),
        (
            "http://rebound.example:8080",
            "rebound.example:8080",
            StatusCode::FORBIDDEN,
        ),
        ("http://192.0.2.7:8080", "192.0.2.7:8080", StatusCode::OK),
    ] {
        let response = send(
            &server,
            Method::GET,
            "/v1/health",
            &[
                ("origin", origin),
                ("host", host),
                ("authorization", &bearer),
            ],
        )
        .await;
        assert_eq!(response.status(), status, "{origin}");
    }
    let plain = send(
        &server,
        Method::GET,
        "/v1/health",
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(plain.status(), StatusCode::OK);
    assert_eq!(header(&plain, "vary"), Some("Origin"));
    assert_eq!(header(&plain, "access-control-allow-origin"), None);

    let mut listed = Server::prepare(true, |_| {});
    listed.set_environment("LEMONADE_ALLOWED_ORIGINS", "https://app.example.com");
    listed.start().await;
    let trusted = send(
        &listed,
        Method::OPTIONS,
        "/v1/chat/completions",
        &[
            ("origin", "https://app.example.com"),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "authorization"),
        ],
    )
    .await;
    assert_eq!(trusted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        header(&trusted, "access-control-allow-origin"),
        Some("https://app.example.com")
    );
    let proxied = send(
        &listed,
        Method::GET,
        "/v1/health",
        &[
            ("origin", "https://juiced.example.net"),
            ("host", "juiced.example.net"),
            ("authorization", &bearer),
        ],
    )
    .await;
    assert_eq!(
        proxied.status(),
        StatusCode::FORBIDDEN,
        "an explicit list is authoritative"
    );
}

#[tokio::test]
async fn an_admin_key_alone_keeps_read_and_inference_open() {
    let mut server = Server::prepare(false, |_| {});
    server.set_environment("LEMONADE_ADMIN_API_KEY", ADMIN_KEY);
    server.start().await;
    for key in [None, Some("invalid-key"), Some(ADMIN_KEY)] {
        for path in [
            "/v1/health",
            "/api/v1/models",
            "/v1/system-info",
            "/metrics",
        ] {
            let response = server.request(Method::GET, path, None, key).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path} {key:?}");
        }
        let (status, reply) = server
            .json(
                Method::POST,
                "/v1/chat/completions",
                Some(json!({"model":"fixture.one","messages":[{"role":"user","content":"open"}]})),
                key,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
    }
    for key in [None, Some("invalid-key")] {
        for (method, path, body) in [
            (Method::GET, "/v1/juiced/requests", None),
            (
                Method::POST,
                "/v1/unload",
                Some(json!({"model":"fixture.one"})),
            ),
        ] {
            let response = server.request(method, path, body, key).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path} {key:?}"
            );
        }
    }
    let (status, _) = server
        .json(Method::GET, "/v1/juiced/requests", None, Some(ADMIN_KEY))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn metrics_use_native_names_and_parse_as_prometheus_text() {
    let server = Server::new(true).await;
    for key in [None, Some("invalid-key")] {
        let response = server
            .request(Method::GET, "/metrics", None, key)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":"fixture.one","messages":[{"role":"user","content":"count me"}]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = server
        .request(Method::GET, "/metrics", None, Some(CLIENT_KEY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "content-type"),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    let text = response.text().await.unwrap();
    let mut typed = std::collections::BTreeMap::new();
    let mut samples = std::collections::BTreeMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').unwrap();
            assert!(matches!(kind, "gauge" | "counter"), "{line}");
            assert!(
                typed.insert(name.to_owned(), kind.to_owned()).is_none(),
                "{line}"
            );
            continue;
        }
        if line.starts_with("# HELP ") {
            continue;
        }
        let (series, value) = line.rsplit_once(' ').unwrap();
        let name = series.split('{').next().unwrap();
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "{line}"
        );
        assert!(typed.contains_key(name), "sample before its TYPE: {line}");
        if series.contains('{') {
            assert!(series.ends_with('}'), "{line}");
        }
        samples.insert(series.to_owned(), value.parse::<f64>().unwrap());
    }
    for name in [
        "lemonade_server_up",
        "lemonade_server_info",
        "lemonade_loaded_models",
        "lemonade_model_info",
        "lemonade_model_loaded",
        "lemonade_model_requests_total",
        "lemonade_requests_total",
        "lemonade_input_tokens_total",
        "lemonade_output_tokens_total",
        "lemonade_cpu_usage_percent",
        "lemonade_memory_used_gb",
    ] {
        assert!(typed.contains_key(name), "{name} is not exposed");
    }
    assert_eq!(typed["lemonade_requests_total"], "counter");
    assert_eq!(samples["lemonade_server_up"], 1.0);
    assert_eq!(samples["lemonade_requests_total"], 1.0);
    assert!(samples.iter().any(|(series, value)| {
        series.starts_with("lemonade_model_requests_total{model_name=\"fixture.one\"")
            && *value == 1.0
    }));
}

async fn log_socket(server: &Server, path: &str, key: Option<&str>) -> Result<Socket, String> {
    let url = format!("{}{path}", server.base.replacen("http://", "ws://", 1));
    let mut request = url.into_client_request().unwrap();
    if let Some(key) = key {
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {key}")).unwrap(),
        );
    }
    connect_async(request)
        .await
        .map(|(socket, _)| socket)
        .map_err(|error| error.to_string())
}

async fn receive(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("log stream message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test]
async fn log_stream_sends_a_snapshot_then_live_entries_on_every_path() {
    let server = Server::new(true).await;
    for key in [None, Some("invalid-key")] {
        let error = log_socket(&server, "/logs/stream", key).await.unwrap_err();
        assert!(error.contains("401"), "{error}");
    }
    for path in ["/logs/stream", "/api/v1/logs/stream", "/v1/logs/stream"] {
        let mut socket = log_socket(&server, path, Some(CLIENT_KEY)).await.unwrap();
        socket
            .send(Message::text(json!({"type":"hello"}).to_string()))
            .await
            .unwrap();
        let error = receive(&mut socket).await;
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["message"], "Expected logs.subscribe message");
        socket
            .send(Message::text(
                json!({"type":"logs.subscribe","after_seq":null}).to_string(),
            ))
            .await
            .unwrap();
        let snapshot = receive(&mut socket).await;
        assert_eq!(snapshot["type"], "logs.snapshot");
        let entries = snapshot["entries"].as_array().unwrap();
        let first = &entries[0];
        assert_eq!(first["seq"], 1);
        assert_eq!(first["tag"], "service");
        assert_eq!(first["severity"], "Info");
        assert!(first["line"].as_str().unwrap().ends_with(&format!(
            "[Info] (service) Service listening on {}",
            server.base
        )));
        let marker = format!("https://marker{}.example", path.len());
        send(&server, Method::GET, "/v1/health", &[("origin", &marker)]).await;
        let live = receive(&mut socket).await;
        assert_eq!(live["type"], "logs.entry");
        assert_eq!(live["entry"]["severity"], "Warn");
        assert!(live["entry"]["line"].as_str().unwrap().contains(&marker));
        let resumed_after = live["entry"]["seq"].as_u64().unwrap() - 1;
        socket
            .send(Message::text(
                json!({"type":"logs.subscribe","after_seq":resumed_after}).to_string(),
            ))
            .await
            .unwrap();
        let resumed = receive(&mut socket).await;
        assert_eq!(resumed["entries"].as_array().unwrap().len(), 1);
        assert_eq!(resumed["entries"][0], live["entry"]);
    }
}

#[tokio::test]
async fn service_and_worker_lines_reach_stderr_at_the_configured_level() {
    let server = Server::prepare(false, |config| {
        config["log_level"] = json!("warning");
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--startup-log"), json!("stderr-marker")]);
    });
    for (level, expect_info) in [(None, false), (Some("info"), true)] {
        let mut command = std::process::Command::new(binary("lemond", "JUICED_TEST_SERVER"));
        command
            .arg("--config")
            .arg(&server.config_path)
            .args(["--listen", "127.0.0.1:0"])
            .env_remove("LEMONADE_API_KEY")
            .env_remove("LEMONADE_ADMIN_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match level {
            Some(level) => command.env("LEMONADE_LOG_LEVEL", level),
            None => command.env_remove("LEMONADE_LOG_LEVEL"),
        };
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (base_sender, base) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(index) = line.find("http://127.0.0.1:") {
                    let _ = base_sender.send(line[index..].to_owned());
                }
            }
        });
        let (line_sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = line_sender.send(line);
            }
        });
        let base = base.recv_timeout(Duration::from_secs(10)).unwrap();
        let reply = reqwest::Client::new()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({"model":"fixture.one","messages":[{"role":"user","content":"log"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        let warning = |seen: &[String]| {
            seen.iter()
                .any(|line| line.ends_with("[Warn] (fixture.one) W fixture stderr stderr-marker"))
        };
        let output = |seen: &[String]| {
            seen.iter()
                .any(|line| line.ends_with("[Info] (fixture.one) fixture stdout stderr-marker"))
        };
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !warning(&seen) || (expect_info && !output(&seen)) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            seen.push(
                lines
                    .recv_timeout(remaining)
                    .unwrap_or_else(|_| panic!("worker line missing from stderr: {seen:?}")),
            );
        }
        let stamped = |line: &String| {
            let bytes = line.as_bytes();
            bytes.len() > 24
                && bytes[4] == b'-'
                && bytes[10] == b' '
                && bytes[19] == b'.'
                && bytes[23] == b' '
        };
        assert!(seen.iter().all(stamped), "{seen:?}");
        let info = seen
            .iter()
            .any(|line| line.contains("[Info] (service) Service listening on"));
        assert_eq!(info, expect_info, "{level:?}: {seen:?}");
        assert_eq!(output(&seen), expect_info, "{level:?}: {seen:?}");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
