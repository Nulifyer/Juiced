#![cfg(target_os = "linux")]

mod support;

use reqwest::{Method, StatusCode};
use serde_json::json;
use support::{CLIENT_KEY, Server};

#[tokio::test]
async fn classify_forwards_to_the_worker_and_answers_with_native_shapes() {
    let server = Server::configured(true, |config| {
        config["models"][1]["operations"] = json!(["classify"]);
    })
    .await;
    for path in [
        "/v1/classify",
        "/api/v1/classify",
        "/v0/classify",
        "/api/v0/classify",
    ] {
        let (status, reply) = server
            .json(
                Method::POST,
                path,
                Some(json!({"model": "fixture.two", "input": "Please verify your account now."})),
                Some(CLIENT_KEY),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {reply}");
        assert_eq!(reply["object"], "classification");
        assert_eq!(reply["model"], "fixture.two");
        assert_eq!(reply["labels"]["phishing"], 0.9);
    }
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/classify",
            Some(json!({"model": "fixture.two", "text": "hello", "top_k": 1})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["labels"], json!({"safe": 0.8}));
    for (body, message) in [
        (
            json!({"model": "fixture.two"}),
            "Missing 'input' (or 'text') string in classify request",
        ),
        (
            json!({"model": "fixture.two", "input": "x", "top_k": 0}),
            "'top_k' must be a positive integer",
        ),
        (
            json!({"input": "x"}),
            "No 'model' specified and no single classification model is loaded (load one, or name it in the request)",
        ),
    ] {
        let (status, reply) = server
            .json(Method::POST, "/v1/classify", Some(body), Some(CLIENT_KEY))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reply["error"]["message"], message);
        assert_eq!(reply["error"]["type"], "invalid_request_error");
    }
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/classify",
            Some(json!({"model": "fixture.one", "input": "x"})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{reply}");
    assert_eq!(reply["error"]["code"], "unsupported_operation");
    let anonymous = server
        .request(
            Method::POST,
            "/v1/classify",
            Some(json!({"model": "fixture.two", "input": "x"})),
            None,
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn engines_list_text_engines_with_unsupported_reasons() {
    let server = Server::new(true).await;
    let engines = server.get("juiced/engines").await;
    let rows = engines["installable"].as_array().unwrap();
    for (engine, backend) in [
        ("vllm", "cuda"),
        ("vllm", "rocm"),
        ("ds4", "rocm"),
        ("llama.cpp", "hrx"),
        ("onnxruntime", "cpu"),
    ] {
        let row = rows
            .iter()
            .find(|row| row["engine"] == engine && row["backend"] == backend)
            .unwrap_or_else(|| panic!("{engine} {backend} missing"));
        if row["availability"] == "unavailable" {
            assert!(
                !row["reasons"].as_array().unwrap().is_empty(),
                "{engine} {backend} has no reason"
            );
        }
    }
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/juiced/engines/install",
            Some(json!({"engine": "nope", "backend": "cpu", "expected_generation": 0})),
            Some(support::ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(reply["error"]["message"].as_str().unwrap().contains("vllm"));
}
