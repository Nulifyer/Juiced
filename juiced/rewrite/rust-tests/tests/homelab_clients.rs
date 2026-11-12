mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use support::{
    Server,
    voice::{self, Wyoming},
};

const HA_PLACEHOLDER_KEY: &str = "sk-0000000000000000000";

fn device_tools() -> Value {
    json!([{"type":"function","function":{"name":"HassTurnOn","description":"Turns on a device.","parameters":{"type":"object","properties":{"name":{"type":"string"}}}}}])
}

#[tokio::test]
async fn llama_cpp_integration_and_healthcheck_work_without_configured_keys() {
    let server = Server::configured(false, |config| {
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .push(json!("--finite-stream"));
    })
    .await;
    let health = server
        .client
        .get(format!("{}/v1/juiced/state", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let (status, models) = server
        .json(Method::GET, "/v1/models", None, Some(HA_PLACEHOLDER_KEY))
        .await;
    assert_eq!(status, StatusCode::OK, "{models}");
    let ids: Vec<_> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"fixture.one"), "{ids:?}");
    assert!(!ids.contains(&"fixture.disabled"));
    let body = json!({
        "messages":[{"role":"system","content":"You are a helpful assistant."},{"role":"user","content":"What is the capital of France?"}],
        "model":"fixture.one","tools":device_tools(),"max_tokens":3000,"top_p":1.0,
        "temperature":0.7,"user":"01JCONVERSATION","stream":true,
    });
    let response = server
        .request(
            Method::POST,
            "/v1/chat/completions",
            Some(body),
            Some(HA_PLACEHOLDER_KEY),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("text/event-stream")
    );
    let text = response.text().await.unwrap();
    assert!(text.contains("data: "), "{text}");
}

#[tokio::test]
async fn ollama_integration_requests_resolve_saved_latest_names_and_tool_turns() {
    let server = Server::configured(true, |config| {
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .push(json!("--finite-stream"));
    })
    .await;
    let (status, tags) = server
        .json(Method::GET, "/api/tags", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::OK, "{tags}");
    let names: Vec<_> = tags["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["model"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"fixture.one:latest"), "{names:?}");
    let (status, version) = server
        .json(Method::GET, "/api/version", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(version, json!({"version":"0.16.1"}));
    let (status, show) = server
        .json(
            Method::POST,
            "/api/show",
            Some(json!({"model":"fixture.one:latest"})),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{show}");
    let body = json!({
        "model":"fixture.one:latest",
        "messages":[
            {"role":"system","content":"You control the home."},
            {"role":"user","content":"Turn on the kitchen."},
            {"role":"assistant","thinking":"Use the tool.","tool_calls":[{"function":{"name":"HassTurnOn","arguments":{"name":"kitchen"}}}]},
            {"role":"tool","content":"{\"data\":{\"success\":true},\"error\":null}"}
        ],
        "tools":device_tools(),"stream":true,"keep_alive":-1,"options":{"num_ctx":8192},"think":true,
    });
    let response = server
        .request(Method::POST, "/api/chat", Some(body), server.read_key())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let last = lines.last().unwrap();
    assert_eq!(last["done"], true, "{text}");
    assert_eq!(last["model"], "fixture.one");
    assert!(
        lines
            .iter()
            .all(|line| line["message"]["role"] == "assistant")
    );
}

#[tokio::test]
async fn native_wyoming_environment_serves_the_wyoming_openai_identity() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap().to_string();
    drop(free);
    let mut server = Server::prepare(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("asr4all");
        model["operations"] = json!(["audio/transcriptions"]);
        model["max_inflight"] = json!(1);
    });
    server.set_environment("JUICED_WYOMING_ENABLED", "true");
    server.set_environment("JUICED_WYOMING_URI", format!("tcp://{address}"));
    server.start().await;
    let view = server.get("juiced/wyoming").await;
    assert_eq!(view["settings"]["enabled"], true);
    assert_eq!(view["listener"]["address"], address);
    let mut client = Wyoming::connect(&address).await;
    client.send("describe", json!({}), &[]).await;
    let info = client.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(info.kind, "info");
    let asr = &info.data["asr"][0];
    let port = server.base.rsplit(':').next().unwrap();
    assert_eq!(asr["name"], "openai");
    assert_eq!(asr["version"], "0.8.0");
    assert_eq!(asr["description"], "OpenAI (Non-Streaming)");
    assert_eq!(
        asr["attribution"],
        json!({"name":"OpenAI-Compatible Proxy","url":format!("http://127.0.0.1:{port}/v1")})
    );
    assert_eq!(asr["requires_external_vad"], true);
    assert_eq!(asr["models"][0]["name"], "fixture.one");
    assert_eq!(asr["models"][0]["languages"], json!(["en"]));
    assert_eq!(asr["models"].as_array().unwrap().len(), 1);
    assert_eq!(info.data["tts"], json!([]));

    let pcm = voice::pcm(&voice::tone(16_000, 300, 0.3));
    let transcript = client.transcribe("en", 16_000, 2, 1, &pcm).await;
    assert_eq!(transcript.kind, "transcript", "{:?}", transcript.data);
    assert_eq!(transcript.data["text"], "Fixture speech λ");
    let refused = client.transcribe("fr", 16_000, 2, 1, &pcm).await;
    assert_eq!(refused.kind, "error");
    assert_eq!(refused.data["code"], "unsupported_transcription_option");

    let pinned = voice::wyoming(&server, json!({"enabled": false})).await;
    assert_eq!(pinned["settings"]["enabled"], true);
}
