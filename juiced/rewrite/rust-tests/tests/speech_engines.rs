mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Server,
    speech::{self, Part},
};

async fn server(provider: &str, args: Value) -> Server {
    Server::configured(true, |config| {
        let events = config["models"][0]["args"][5].clone();
        let model = &mut config["models"][0];
        model["provider"] = json!(provider);
        model["operations"] = json!(["audio/transcriptions"]);
        model["max_inflight"] = json!(1);
        let mut args = args;
        args.as_array_mut()
            .unwrap()
            .extend([json!("--events"), events]);
        model["args"] = args;
    })
    .await
}

async fn transcribe(server: &Server, file: &[u8], fields: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut parts = vec![
        Part::File("file", "probe.wav", "audio/wav", file),
        Part::Text("model", "fixture.one"),
    ];
    parts.extend(fields.iter().map(|(name, value)| Part::Text(name, value)));
    let response = speech::send(
        server,
        "/v1/audio/transcriptions",
        &parts,
        server.read_key(),
    )
    .await;
    let status = response.status();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn startup(server: &Server) -> Value {
    let events =
        std::fs::read_to_string(server.directory.path().join("fixture.one.events")).unwrap();
    events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|value| value["id"] == "worker-startup")
        .unwrap()
}

#[tokio::test]
async fn parakeet_worker_receives_only_its_openai_route_wav_and_format() {
    let server = server(
        "parakeet",
        json!(["--port", "{port}", "--model", "{model}"]),
    )
    .await;
    let audio = speech::audio();
    let (status, value) = transcribe(
        &server,
        &audio,
        &[
            ("response_format", "verbose_json"),
            ("language", "de"),
            ("prompt", "private prompt"),
            ("temperature", "0.5"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["text"], "Fixture speech λ");
    assert_eq!(
        value["received_fields"],
        json!({"response_format":"verbose_json"})
    );
    assert_eq!(value["received_file"]["content_type"], "audio/wav");
    assert_eq!(startup(&server)["host"], "127.0.0.1");
    let (status, value) = transcribe(&server, &audio, &[("response_format", "srt")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], "unsupported_transcription_option");
    let (status, value) = transcribe(&server, b"OggS-not-wave", &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], "unsupported_transcription_option");
    let state = server.get("juiced/state").await;
    assert_eq!(state["resources"]["active_requests"], 0);
}

#[tokio::test]
async fn futo_worker_gets_english_json_or_text_without_a_host_argument() {
    let server = server(
        "asr4all",
        json!([
            "--port",
            "{port}",
            "--model",
            "{model}",
            "--threads",
            "2",
            "--futo-commit",
            "16",
            "--futo-right",
            "4"
        ]),
    )
    .await;
    let audio = speech::audio();
    let (status, value) = transcribe(&server, &audio, &[("response_format", "json")]).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        value["received_fields"],
        json!({"response_format":"json","language":"auto"})
    );
    assert!(startup(&server)["host"].is_null());
    let (status, value) = transcribe(&server, &audio, &[("response_format", "text")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, "Fixture speech λ\n");
    for fields in [
        [("response_format", "verbose_json"), ("language", "en")],
        [("response_format", "json"), ("language", "fr")],
        [("response_format", "json"), ("prompt", "bias")],
        [("response_format", "json"), ("temperature", "0.3")],
    ] {
        let (status, value) = transcribe(&server, &audio, &fields).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{fields:?}");
        assert_eq!(value["error"]["code"], "unsupported_transcription_option");
    }
}

#[tokio::test]
async fn moonshine_worker_gets_a_model_directory_and_private_streaming_ports() {
    let server = server(
        "moonshine",
        json!([
            "--port",
            "{port}",
            "--model-path",
            "{model}",
            "--model-arch",
            "2"
        ]),
    )
    .await;
    let (status, value) = transcribe(
        &server,
        b"fLaC-not-wave",
        &[("response_format", "srt"), ("language", "en")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(value.as_str().unwrap().contains("Fixture speech"));
    let started = startup(&server);
    assert_eq!(
        std::path::Path::new(started["model_path"].as_str().unwrap()),
        server.directory.path()
    );
    let port = started["port"].as_u64().unwrap();
    let ports: Vec<u64> = ["ws_port", "tcp_port"]
        .iter()
        .map(|key| started[*key].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(ports.iter().all(|other| *other != port && *other > 0));
    assert_ne!(ports[0], ports[1]);
    assert!(started["host"].is_null());
}

#[tokio::test]
async fn speech_engine_rows_are_listed_with_their_install_sources() {
    let server = Server::configured(true, |_| {}).await;
    let engines = server.get("juiced/engines").await;
    let row = |engine: &str| {
        engines["installable"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["engine"] == engine && row["backend"] == "cpu")
            .cloned()
            .unwrap()
    };
    let futo = row("futo-transcribe.cpp");
    assert_eq!(futo["unresolved"], "not_published");
    assert!(futo["build"].is_null());
    let parakeet = row("parakeet.cpp");
    assert_eq!(parakeet["build"]["repository"], "mudler/parakeet.cpp");
    assert_eq!(parakeet["build"]["tag"], "v0.6.1");
    let moonshine = row("moonshine");
    assert_eq!(moonshine["build"]["tag"], "moonshine0.0.62");
    assert!(engines["recommendations"].get("parakeet").is_some());
    let (status, refused) = server
        .post(
            "juiced/engines/install",
            json!({"engine":"futo-transcribe.cpp","backend":"cpu","expected_generation":engines["generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refused["error"]["code"], "engine_not_published");
}
