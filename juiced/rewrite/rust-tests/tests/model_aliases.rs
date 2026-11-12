mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server};
use tokio::io::AsyncReadExt;

async fn set(server: &Server, alias: &str, target: Value, revision: &Value) -> (StatusCode, Value) {
    server
        .post(
            "juiced/aliases",
            json!({"alias":alias,"target":target,"expected_revision":revision}),
        )
        .await
}

async fn inference(server: &Server, path: &str, body: Value) -> (StatusCode, Value) {
    server
        .json(Method::POST, path, Some(body), server.read_key())
        .await
}

fn listed<'a>(models: &'a Value, key: &str, id: &str) -> Option<&'a Value> {
    models
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model[key] == id)
}

async fn read_response(mut socket: tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut part = [0; 4096];
            let count = socket.read(&mut part).await.unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&part[..count]);
            if bytes.ends_with(b"\r\n0\r\n\r\n")
                || (bytes.windows(4).any(|window| window == b"\r\n\r\n") && bytes.ends_with(b"}"))
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn aliases_route_every_protocol_swap_without_interrupting_in_flight_work_and_persist() {
    let mut server = Server::new(true).await;
    let initial = server.get("juiced/aliases").await;
    assert_eq!(initial["revision"], 0);
    assert!(initial["aliases"].as_array().unwrap().is_empty());
    for (alias, target, status, code) in [
        (
            "fixture.two",
            json!("fixture.one"),
            StatusCode::CONFLICT,
            "alias_conflict",
        ),
        (
            "chat",
            json!("absent"),
            StatusCode::NOT_FOUND,
            "unknown_model",
        ),
        (
            "",
            json!("fixture.one"),
            StatusCode::BAD_REQUEST,
            "invalid_alias",
        ),
        ("chat", Value::Null, StatusCode::NOT_FOUND, "unknown_alias"),
    ] {
        let (actual, error) = set(&server, alias, target, &json!(0)).await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["error"]["code"], code);
    }
    let (status, _) = server
        .post(
            "juiced/aliases",
            json!({"alias":"chat","expected_revision":0}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, created) = set(&server, "chat", json!("fixture.one"), &json!(0)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let first = created["revision"].clone();
    assert_ne!(first, 0);
    assert_eq!(
        created["aliases"],
        json!([{"alias":"chat","target":"fixture.one","target_state":"unloaded","target_policy":"on-demand","available":true}])
    );
    let (status, stale) = set(&server, "chat", json!("fixture.two"), &json!(0)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "revision_conflict");
    for prefix in PREFIXES {
        let (status, listed) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/aliases"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["revision"], first);
    }

    let models = server.get("models").await;
    let entry = listed(&models["data"], "id", "chat").expect("alias in model list");
    assert_eq!(entry["owned_by"], "juiced-alias");
    assert_eq!(entry["aliased_to"], "fixture.one");
    assert_eq!(
        listed(&models["data"], "id", "fixture.one").unwrap()["owned_by"],
        "juiced"
    );
    let detail = server.get("models/chat").await;
    assert_eq!(detail["aliased_to"], "fixture.one");
    assert_eq!(detail["juiced"]["id"], "fixture.one");
    let (status, tags) = server
        .json(Method::GET, "/api/tags", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed(&tags["models"], "name", "chat").expect("alias in Ollama tags")["aliased_to"],
        "fixture.one"
    );

    let (status, reply) = inference(
        &server,
        "/v1/chat/completions",
        json!({"model":"chat","test_id":"openai-alias","messages":[{"role":"user","content":"alias"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], "chat");
    assert!(server.event("fixture.one", "openai-alias", "completed"));
    assert!(!server.event("fixture.two", "openai-alias", "started"));
    let (status, reply) = inference(
        &server,
        "/api/chat",
        json!({"model":"chat","messages":[{"role":"user","content":"alias"}],"stream":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], "chat");
    assert_eq!(reply["done"], true);
    let (status, reply) = inference(
        &server,
        "/v1/messages",
        json!({"model":"chat","messages":[{"role":"user","content":"alias"}],"max_tokens":4}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], "chat");
    let (status, reply) = inference(
        &server,
        "/mcp",
        json!({"jsonrpc":"2.0","id":"alias","method":"tools/call","params":{"name":"lemonade_chat","arguments":{"model":"chat","messages":[{"role":"user","content":"alias"}]}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    assert_eq!(reply["result"]["structuredContent"]["model"], "chat");
    let records = server.get("juiced/requests").await;
    let aliased: Vec<_> = records["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["alias"] == "chat")
        .collect();
    assert_eq!(aliased.len(), 4);
    assert!(
        aliased
            .iter()
            .all(|record| record["model"] == "fixture.one")
    );
    assert!(
        records["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["alias"] != "fixture.one")
    );

    let held = server
        .socket_request(
            "chat/completions",
            json!({"model":"chat","test_id":"held","fixture_delay_ms":800,"messages":[]}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "held", "started"))
        .await;
    let (status, swapped) = set(&server, "chat", json!("fixture.two"), &first).await;
    assert_eq!(status, StatusCode::OK, "{swapped}");
    let second = swapped["revision"].clone();
    assert_eq!(swapped["aliases"][0]["target"], "fixture.two");
    let (status, reply) = inference(
        &server,
        "/v1/chat/completions",
        json!({"model":"chat","test_id":"after-swap","messages":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], "chat");
    assert!(server.event("fixture.two", "after-swap", "completed"));
    assert!(!server.event("fixture.one", "after-swap", "started"));
    let response = read_response(held).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(server.event("fixture.one", "held", "completed"));
    assert!(!server.event("fixture.two", "held", "started"));
    assert_eq!(server.model("fixture.one").await["loaded"], true);

    server.restart().await;
    let restored = server.get("juiced/aliases").await;
    assert_eq!(restored["revision"], second);
    assert_eq!(restored["aliases"][0]["target"], "fixture.two");
    let (status, deleted) = set(&server, "chat", Value::Null, &second).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert!(deleted["aliases"].as_array().unwrap().is_empty());
    assert!(listed(&server.get("models").await["data"], "id", "chat").is_none());
    let (status, error) = inference(
        &server,
        "/v1/chat/completions",
        json!({"model":"chat","messages":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "unknown_model");
    server.restart().await;
    assert!(
        server.get("juiced/aliases").await["aliases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn alias_targets_keep_their_policy_and_cannot_be_removed_or_shadowed() {
    let server = Server::new(true).await;
    let (status, created) = set(&server, "assistant", json!("fixture.disabled"), &json!(0)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["aliases"][0]["available"], false);
    assert_eq!(created["aliases"][0]["target_policy"], "disabled");
    assert!(listed(&server.get("models").await["data"], "id", "assistant").is_none());
    let (status, tags) = server
        .json(Method::GET, "/api/tags", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed(&tags["models"], "name", "assistant").is_none());
    let (status, error) = inference(
        &server,
        "/v1/chat/completions",
        json!({"model":"assistant","messages":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "model_disabled");
    let (status, error) = inference(
        &server,
        "/api/generate",
        json!({"model":"assistant","prompt":"alias","stream":false}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "model_disabled");

    let target = server.model("fixture.disabled").await;
    let (status, error) = server
        .post(
            "delete",
            json!({"model":"fixture.disabled","expected_revision":target["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "alias_target");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("assistant")
    );

    let mut registration = server.config["models"][0].clone();
    registration["id"] = json!("assistant");
    registration["policy"] = json!("disabled");
    let (status, error) = server.post("models/register", registration).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "alias_conflict");

    let (status, repointed) = set(
        &server,
        "assistant",
        json!("fixture.one"),
        &created["revision"],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repointed}");
    assert_eq!(repointed["aliases"][0]["available"], true);
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":"fixture.disabled","expected_revision":target["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    let (status, reply) = inference(
        &server,
        "/api/generate",
        json!({"model":"assistant","prompt":"alias","stream":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], "assistant");
}
