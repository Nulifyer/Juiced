mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use support::{
    ADMIN_KEY, CLIENT_KEY, COMPAT_ROUTES, INFERENCE_ROUTES, MULTIPART_INFERENCE_ROUTES,
    OPERATOR_READ_ROUTES, PREFIXES, READ_ROUTES, Server, WRITE_ROUTES, route_matrix,
};
use tokio::io::AsyncReadExt;

fn concrete_route(path: &str) -> &str {
    match path {
        "/models/{model}" => "/models/fixture.one",
        "/models/{model}/options" => "/models/fixture.one/options",
        "/models/{model}/artifacts" => "/models/fixture.one/artifacts",
        "/models/{model}/runtime" => "/models/fixture.one/runtime",
        "/juiced/suitability" => "/juiced/suitability?model=fixture.one",
        "/juiced/context" => "/juiced/context?model=fixture.one",
        "/juiced/runtimes/resolve" => "/juiced/runtimes/resolve?recipe=llamacpp&backend=cpu",
        "/juiced/artifacts/jobs/{job}" => "/juiced/artifacts/jobs/00000000000000000000000000000000",
        "/juiced/artifacts/sets/{set}" => {
            "/juiced/artifacts/sets/set-0000000000000000000000000000000000000000000000000000000000000000"
        }
        _ => path,
    }
}

#[tokio::test]
async fn endpoint_inventory_matches_exercised_http_contracts() {
    let server = Server::new(true).await;
    let expected = route_matrix();
    let inventory = server.get("juiced/endpoints").await;
    assert_eq!(inventory["schema_version"], 1);
    let entries = inventory["endpoints"]
        .as_array()
        .expect("endpoint inventory array");
    let mut actual = BTreeSet::new();
    for entry in entries {
        let method = entry["method"].as_str().unwrap();
        let path = entry["path"].as_str().unwrap();
        assert!(
            actual.insert((method.to_owned(), path.to_owned())),
            "duplicate endpoint contract"
        );
        let prefixes: Vec<&str> = entry["prefixes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|prefix| prefix.as_str().unwrap())
            .collect();
        if let Some((_, _, authority)) = COMPAT_ROUTES
            .iter()
            .find(|(m, p, _)| *m == method && *p == path)
        {
            assert_eq!(prefixes, [""]);
            assert_eq!(entry["authority"], *authority);
            continue;
        }
        assert_eq!(prefixes, PREFIXES);
        let authority = if method == "GET" && READ_ROUTES.contains(&path) {
            "read"
        } else if INFERENCE_ROUTES.contains(&path) || MULTIPART_INFERENCE_ROUTES.contains(&path) {
            "inference"
        } else {
            "operator"
        };
        assert_eq!(
            entry["authority"], authority,
            "authority for {method} {path}"
        );
    }
    assert_eq!(
        actual, expected,
        "Endpoint inventory changed. Add a real HTTP behavior and failure scenario to rust-tests before accepting the route."
    );
}

#[tokio::test]
async fn all_prefixes_enforce_read_inference_and_operator_authority() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        for suffix in READ_ROUTES {
            let path = format!("{prefix}{}", concrete_route(suffix));
            for method in [Method::GET, Method::HEAD] {
                for token in [None, Some("invalid-key")] {
                    let response = server
                        .request(method.clone(), &path, None, token)
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::UNAUTHORIZED,
                        "{method} {path}"
                    );
                    assert!(response.headers().contains_key("www-authenticate"));
                }
                for token in [CLIENT_KEY, ADMIN_KEY] {
                    let response = server
                        .request(method.clone(), &path, None, Some(token))
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
                    if method == Method::HEAD {
                        assert!(response.bytes().await.unwrap().is_empty());
                    } else {
                        response.json::<Value>().await.unwrap();
                    }
                }
            }
        }
        for suffix in WRITE_ROUTES {
            let path = format!("{prefix}{suffix}");
            for token in [None, Some(CLIENT_KEY), Some("invalid-key")] {
                assert_eq!(
                    server
                        .request(
                            Method::POST,
                            &path,
                            Some(json!({"model":"fixture.one"})),
                            token
                        )
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::UNAUTHORIZED,
                    "POST {path}"
                );
            }
            let malformed = server
                .request(Method::POST, &path, Some(json!({})), Some(ADMIN_KEY))
                .await
                .unwrap();
            assert!(
                malformed.status().is_client_error(),
                "POST {path} must reject missing fields"
            );
            assert_ne!(malformed.status(), StatusCode::UNAUTHORIZED);
        }
        for suffix in OPERATOR_READ_ROUTES {
            let path = format!("{prefix}{}", concrete_route(suffix));
            for method in [Method::GET, Method::HEAD] {
                for token in [None, Some(CLIENT_KEY), Some("invalid-key")] {
                    let response = server
                        .request(method.clone(), &path, None, token)
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::UNAUTHORIZED,
                        "{method} {path}"
                    );
                    assert!(response.headers().contains_key("www-authenticate"));
                    if token == Some(CLIENT_KEY) && method == Method::GET {
                        assert_eq!(
                            response.json::<Value>().await.unwrap()["error"]["code"],
                            "operator_required"
                        );
                    }
                }
                let response = server
                    .request(method.clone(), &path, None, Some(ADMIN_KEY))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if suffix.ends_with("/{job}") || suffix.ends_with("/{set}") {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::OK
                    },
                    "{method} {path}"
                );
                if method == Method::HEAD {
                    assert!(response.bytes().await.unwrap().is_empty());
                } else {
                    let value = response.json::<Value>().await.unwrap();
                    if suffix.ends_with("/{job}") || suffix.ends_with("/{set}") {
                        assert_eq!(value["error"]["code"], "artifact_not_found");
                    }
                }
            }
        }
        for suffix in INFERENCE_ROUTES {
            let path = format!("{prefix}{suffix}");
            assert_eq!(
                server
                    .request(Method::POST, &path, Some(json!({})), None)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                server
                    .json(Method::POST, &path, Some(json!({})), Some(CLIENT_KEY))
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
            let (status, response) = server.json(Method::POST, &path, Some(json!({"model":"fixture.one", "messages":[{"role":"user","content":"pipeline only"}]})), Some(CLIENT_KEY)).await;
            assert_eq!(status, StatusCode::OK, "POST {path}");
            if suffix == "/embeddings" {
                assert_eq!(
                    response["data"][0]["embedding"].as_array().unwrap().len(),
                    2
                );
            } else {
                assert_eq!(response["model"], "fixture.one");
                assert_eq!(response["received_authorization"], false);
                assert_eq!(response["manager_key_present"], false);
            }
        }
    }
}

#[tokio::test]
async fn native_read_contracts_keep_observation_and_intent_distinct() {
    let server = Server::new(false).await;
    let health = server.get("health").await;
    assert_eq!(health["status"], "ok");
    let state = server.get("juiced/state").await;
    assert_eq!(state["schema_version"], 1);
    assert!(
        state["version"]
            .as_str()
            .is_some_and(|version| !version.is_empty())
    );
    assert_eq!(
        state["resources"]["reservation_source"],
        "operator-declared"
    );
    assert_eq!(state["resources"]["measured_memory_bytes"], Value::Null);
    let models = server.get("models").await;
    assert_eq!(models["object"], "list");
    assert_eq!(models["data"].as_array().unwrap().len(), 2);
    assert!(!models.to_string().contains("fixture.disabled"));
    let catalog = server.get("juiced/catalog").await;
    assert_eq!(catalog["data"].as_array().unwrap().len(), 3);
    assert!(
        catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model["loaded"] == false)
    );
    let protocols = server.get("juiced/protocols").await;
    assert!(
        protocols["data"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty())
    );
    let history = server.get("juiced/requests").await;
    assert_eq!(history["capacity"], 128);
    assert_eq!(history["contains_content"], false);
    assert_eq!(history["data"], json!([]));
}

#[tokio::test]
async fn policy_changes_survive_process_restart_and_reject_stale_writes() {
    let mut server = Server::new(true).await;
    let initial = server.model("fixture.one").await;
    let expected_revision = initial["revision"].as_u64().unwrap();
    let (status, saved) = server.post("juiced/policy", json!({"model":"fixture.one", "policy":"disabled", "expected_revision":expected_revision})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["revision"], expected_revision + 1);
    assert_eq!(server.post("juiced/policy", json!({"model":"fixture.one", "policy":"hot", "expected_revision":expected_revision})).await.0, StatusCode::CONFLICT);
    server.restart().await;
    let restored = server.model("fixture.one").await;
    assert_eq!(restored["policy"], "disabled");
    assert_eq!(restored["revision"], expected_revision + 1);
    assert_eq!(restored["loaded"], false);
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.1["error"]["code"],
        "model_disabled"
    );
    assert_eq!(
        server
            .json(
                Method::POST,
                "/v1/chat/completions",
                Some(json!({"model":"fixture.one"})),
                server.read_key()
            )
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn model_lifecycle_is_serialized_and_hot_and_busy_guards_are_enforced() {
    let server = Server::new(true).await;
    let (left, right) = tokio::join!(
        server.post("load", json!({"model":"fixture.one"})),
        server.post("load", json!({"model":"fixture.one"}))
    );
    assert_eq!(left.0, StatusCode::OK);
    assert_eq!(right.0, StatusCode::OK);
    assert_eq!(left.1["pid"], right.1["pid"]);
    let pid = left.1["pid"].clone();
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        64
    );
    assert_eq!(
        server
            .post(
                "juiced/policy",
                json!({"model":"fixture.one", "policy":"disabled", "expected_revision":0})
            )
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        server
            .post(
                "juiced/policy",
                json!({"model":"fixture.one", "policy":"hot", "expected_revision":0})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .1["error"]["code"],
        "worker_hot"
    );
    assert_eq!(
        server
            .post(
                "juiced/policy",
                json!({"model":"fixture.one", "policy":"on-demand", "expected_revision":1})
            )
            .await
            .0,
        StatusCode::OK
    );
    let socket = server
        .socket_request(
            "chat/completions",
            json!({"model":"fixture.one", "test_id":"busy", "fixture_delay_ms":2000}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "busy", "started"))
        .await;
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .1["error"]["code"],
        "worker_busy"
    );
    drop(socket);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    assert_eq!(server.model("fixture.one").await["pid"], pid);
    let (status, unloaded) = server.post("unload", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unloaded["loaded"], false);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn resource_and_slot_pressure_do_not_evict_other_workers() {
    let server =
        Server::configured(false, |config| config["resident_budget_bytes"] = json!(64)).await;
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    let pid = server.model("fixture.one").await["pid"].clone();
    assert_eq!(
        server.post("load", json!({"model":"fixture.two"})).await.1["error"]["code"],
        "resident_budget"
    );
    let one = server
        .socket_request(
            "chat/completions",
            json!({"model":"fixture.one","test_id":"slot-one","fixture_delay_ms":2000}),
        )
        .await;
    let two = server
        .socket_request(
            "chat/completions",
            json!({"model":"fixture.one","test_id":"slot-two","fixture_delay_ms":2000}),
        )
        .await;
    server
        .wait_for(async || {
            server.event("fixture.one", "slot-one", "started")
                && server.event("fixture.one", "slot-two", "started")
        })
        .await;
    let queued = server
        .socket_request(
            "chat/completions",
            json!({"model":"fixture.one","test_id":"slot-queued"}),
        )
        .await;
    server
        .wait_for(async || server.get("juiced/admission").await["scheduler"]["pending"] == 1)
        .await;
    assert!(!server.event("fixture.one", "slot-queued", "started"));
    drop(queued);
    server
        .wait_for(async || server.get("juiced/admission").await["scheduler"]["pending"] == 0)
        .await;
    assert!(!server.event("fixture.one", "slot-queued", "started"));
    assert_eq!(server.model("fixture.one").await["pid"], pid);
    drop(one);
    drop(two);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server.post("load", json!({"model":"fixture.two"})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn suitability_does_not_modify_state_load_workers_or_consume_slots() {
    let server = Server::new(true).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    let state = server.get("juiced/state").await;
    let catalog = server.get("juiced/catalog").await;
    let records = server.get("juiced/requests").await;
    for prefix in PREFIXES {
        let (status, result) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/suitability?model=fixture.disabled"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["read_only"], true);
        assert_eq!(result["reservation"], false);
        assert_eq!(result["enabled"], false);
        assert_eq!(result["resources"]["fit"], "unknown");
        assert_eq!(result["resources"]["hardware_qualified"], false);
        assert_eq!(result["resources"]["recommendation_qualified"], false);
        assert_eq!(result["context_tokens"], Value::Null);
    }
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(
        server.get("juiced/state").await["resources"],
        state["resources"]
    );
    assert_eq!(server.get("juiced/catalog").await, catalog);
    assert_eq!(server.get("juiced/requests").await, records);
}

#[tokio::test]
async fn malformed_requests_are_rejected_without_worker_side_effects() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        for suffix in WRITE_ROUTES.iter().chain(INFERENCE_ROUTES.iter()) {
            let response = server
                .client
                .post(format!("{}{prefix}{suffix}", server.base))
                .bearer_auth(ADMIN_KEY)
                .header("content-type", "application/json")
                .body("{")
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_client_error(),
                "malformed JSON accepted by {suffix}"
            );
            let error: Value = response
                .json()
                .await
                .expect("API extraction failures must return JSON");
            assert!(
                error["error"]["message"].as_str().is_some(),
                "malformed {suffix} response has no structured message"
            );
        }
        for suffix in [
            "/juiced/suitability",
            "/juiced/suitability?model=",
            "/juiced/suitability?model=missing",
        ] {
            assert_eq!(
                server
                    .json(
                        Method::GET,
                        &format!("{prefix}{suffix}"),
                        None,
                        server.admin_key()
                    )
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            server
                .json(
                    Method::GET,
                    &format!("{prefix}/does-not-exist"),
                    None,
                    server.read_key()
                )
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    let oversized = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .bearer_auth(CLIENT_KEY)
        .json(&json!({"model":"fixture.one", "prompt":"x".repeat(2 * 1024 * 1024)}))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: Value = oversized
        .json()
        .await
        .expect("body-limit failures must return JSON");
    assert!(error["error"]["message"].as_str().is_some());
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["active_requests"],
        0
    );
}

#[tokio::test]
async fn unsupported_methods_and_invalid_query_fields_have_structured_errors() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        let (status, error) = server
            .json(
                Method::PUT,
                &format!("{prefix}/health"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(error["error"]["message"].as_str().is_some());
        let (status, error) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/suitability?model=fixture.one&unexpected=true"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"]["message"].as_str().is_some());
    }
}

#[tokio::test]
async fn upstream_errors_and_timeouts_preserve_status_and_release_slots() {
    let server =
        Server::configured(false, |config| config["request_timeout_ms"] = json!(100)).await;
    let (status, failure) = server
        .post(
            "chat/completions",
            json!({"model":"fixture.one", "fixture_status":429}),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(failure["error"]["message"], "fixture failure");
    let pid = server.model("fixture.one").await["pid"].clone();
    assert_eq!(
        server
            .post(
                "chat/completions",
                json!({"model":"fixture.one", "fixture_delay_ms":2000})
            )
            .await
            .0,
        StatusCode::GATEWAY_TIMEOUT
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["active_requests"],
        0
    );
    assert_eq!(server.model("fixture.one").await["pid"], pid);
    assert_eq!(
        server
            .post("chat/completions", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn generation_routes_cancel_upstream_on_nonstreaming_and_streaming_disconnect() {
    let server =
        Server::configured(true, |config| config["request_timeout_ms"] = json!(10000)).await;
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        server.post("load", json!({"model":"fixture.two"})).await.0,
        StatusCode::OK
    );
    let pid = server.model("fixture.one").await["pid"].clone();
    let unrelated = server.model("fixture.two").await["pid"].clone();
    for operation in INFERENCE_ROUTES
        .into_iter()
        .filter(|path| *path != "/embeddings")
    {
        for streaming in [false, true] {
            let id = format!("{operation}-{streaming}");
            let mut body = json!({"model":"fixture.one", "test_id":id, "stream":streaming});
            if !streaming {
                body["fixture_delay_ms"] = json!(5000);
            }
            let mut socket = server
                .socket_request(operation.trim_start_matches('/'), body)
                .await;
            server
                .wait_for(async || server.event("fixture.one", &id, "started"))
                .await;
            if streaming {
                let mut bytes = [0u8; 2048];
                assert!(
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        socket.read(&mut bytes)
                    )
                    .await
                    .unwrap()
                    .unwrap()
                        > 0
                );
                assert!(String::from_utf8_lossy(&bytes).contains("text/event-stream"));
            }
            drop(socket);
            server
                .wait_for(async || {
                    server.get("juiced/state").await["resources"]["active_requests"] == 0
                })
                .await;
            server
                .wait_for(async || server.event("fixture.one", &id, "cancelled"))
                .await;
            assert_eq!(server.model("fixture.one").await["pid"], pid);
            assert_eq!(server.model("fixture.two").await["pid"], unrelated);
        }
    }
}

#[tokio::test]
async fn worker_crash_and_startup_failure_are_scoped_and_recoverable() {
    let server = Server::new(false).await;
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        server.post("load", json!({"model":"fixture.two"})).await.0,
        StatusCode::OK
    );
    let pid = server.model("fixture.one").await["pid"].clone();
    let unrelated = server.model("fixture.two").await["pid"].clone();
    assert_eq!(
        server
            .post(
                "chat/completions",
                json!({"model":"fixture.one", "fixture_crash":true})
            )
            .await
            .0,
        StatusCode::BAD_GATEWAY
    );
    server
        .wait_for(async || server.model("fixture.one").await["error"] == "worker_exited")
        .await;
    assert_eq!(
        server
            .post("chat/completions", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(server.model("fixture.one").await["pid"], pid);
    assert_eq!(server.model("fixture.two").await["pid"], unrelated);
    let failed = Server::configured(false, |config| {
        config["startup_timeout_ms"] = json!(150);
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--mode"), json!("hang")]);
    })
    .await;
    assert_eq!(
        failed.post("load", json!({"model":"fixture.one"})).await.1["error"]["code"],
        "worker_not_ready"
    );
    assert_eq!(
        failed.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(
        failed.post("load", json!({"model":"fixture.two"})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn request_history_is_bounded_and_contains_no_credentials_or_content() {
    let server = Server::new(true).await;
    for _ in 0..132 {
        let (status, _) = server.json(Method::POST, "/v1/chat/completions", Some(json!({"model":"fixture.one", "messages":[{"role":"user", "content":"private pipeline prompt"}]})), server.read_key()).await;
        assert_eq!(status, StatusCode::OK);
    }
    let records = server.get("juiced/requests").await;
    assert_eq!(records["data"].as_array().unwrap().len(), 128);
    assert_eq!(records["data"][0]["id"], 132);
    let encoded = records.to_string();
    assert!(!encoded.contains("private pipeline prompt"));
    assert!(!encoded.contains(CLIENT_KEY));
    assert!(!encoded.contains(ADMIN_KEY));
    assert!(!encoded.contains("Rust fixture reply"));
    assert_eq!(
        server.get("juiced/state").await["resources"]["active_requests"],
        0
    );
}

#[tokio::test]
async fn credential_transports_are_equivalent_and_ambiguous_headers_fail_closed() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        let url = format!("{}{prefix}/juiced/catalog", server.base);
        let response = server
            .client
            .get(&url)
            .header("x-api-key", CLIENT_KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let catalog: Value = response.json().await.unwrap();
        assert_eq!(
            catalog,
            server
                .json(
                    Method::GET,
                    &format!("{prefix}/juiced/catalog"),
                    None,
                    Some(CLIENT_KEY)
                )
                .await
                .1
        );
        assert_eq!(
            server
                .client
                .get(&url)
                .bearer_auth(CLIENT_KEY)
                .header("x-api-key", CLIENT_KEY)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            server
                .client
                .get(&url)
                .bearer_auth(CLIENT_KEY)
                .header("x-api-key", ADMIN_KEY)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            server
                .client
                .get(&url)
                .header("authorization", format!("Bearer {CLIENT_KEY}"))
                .header("authorization", format!("Bearer {ADMIN_KEY}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            server
                .client
                .get(&url)
                .header("x-api-key", CLIENT_KEY)
                .header("x-api-key", ADMIN_KEY)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn inference_forwards_original_json_bytes_including_whitespace_and_escaped_ids() {
    let server = Server::new(true).await;
    let bodies = [
        " \n{ \"opaque\" : { \"b\" : [true,null,5], \"a\":\"café\" }, \"model\" : \"fixture.one\", \"prompt\" : \"new\\nline\" }\r\n",
        r#"{ "model" : "fixture.\u006fne", "prompt" : "preserve escapes: \u0061", "max_tokens" : 3 }"#,
    ];
    for suffix in INFERENCE_ROUTES
        .into_iter()
        .filter(|path| *path != "/embeddings")
    {
        for body in bodies {
            let expected = format!("{:x}", Sha256::digest(body.as_bytes()));
            let response = server
                .client
                .post(format!("{}/v1{suffix}", server.base))
                .bearer_auth(CLIENT_KEY)
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value = response.json().await.unwrap();
            assert_eq!(value["model"], "fixture.one");
            assert_eq!(
                value["received_body_sha256"], expected,
                "{suffix} reserialized or changed the request body"
            );
            assert_eq!(value["received_authorization"], false);
        }
    }
}

#[tokio::test]
async fn policy_and_worker_mutations_function_under_each_native_prefix() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        let revision = server.model("fixture.one").await["revision"]
            .as_u64()
            .unwrap();
        let policy = format!("{prefix}/juiced/policy");
        let load = format!("{prefix}/load");
        let unload = format!("{prefix}/unload");
        assert_eq!(server.json(Method::POST, &policy, Some(json!({"model":"fixture.one","policy":"on-demand","expected_revision":revision})), server.admin_key()).await.0, StatusCode::OK);
        let (status, worker) = server
            .json(
                Method::POST,
                &load,
                Some(json!({"model":"fixture.one"})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(worker["loaded"], true);
        let (status, worker) = server
            .json(
                Method::POST,
                &unload,
                Some(json!({"model":"fixture.one"})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(worker["loaded"], false);
        assert_eq!(server.json(Method::POST, &policy, Some(json!({"model":"fixture.one","policy":"disabled","expected_revision":revision+1})), server.admin_key()).await.0, StatusCode::OK);
        assert_eq!(
            server
                .json(
                    Method::POST,
                    &load,
                    Some(json!({"model":"fixture.one"})),
                    server.admin_key()
                )
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(server.json(Method::POST, &policy, Some(json!({"model":"fixture.one","policy":"on-demand","expected_revision":revision+2})), server.admin_key()).await.0, StatusCode::OK);
    }
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(server.model("fixture.disabled").await["policy"], "disabled");
}
