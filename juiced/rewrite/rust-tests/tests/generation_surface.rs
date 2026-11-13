mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{CLIENT_KEY, Server};

/// Points model `index` at the fixture worker as one generation engine.
fn worker(
    config: &mut Value,
    index: usize,
    id: &str,
    provider: &str,
    operation: &str,
    args: Value,
) {
    let model = &mut config["models"][index];
    let events = model["args"][5].clone();
    model["id"] = json!(id);
    model["provider"] = json!(provider);
    model["operations"] = json!([operation]);
    model["max_inflight"] = json!(1);
    let mut args = args;
    args.as_array_mut()
        .unwrap()
        .extend([json!("--events"), events]);
    model["args"] = args;
}

async fn server(provider: &str, operation: &str, args: Value) -> Server {
    Server::configured(true, |config| {
        worker(config, 0, "fixture.one", provider, operation, args)
    })
    .await
}

async fn post(server: &Server, path: &str, body: Value) -> (StatusCode, String, Vec<u8>) {
    let response = server
        .client
        .post(format!("{}{path}", server.base))
        .bearer_auth(CLIENT_KEY)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    (
        status,
        content_type,
        response.bytes().await.unwrap().to_vec(),
    )
}

/// The fixture's WAV data chunk carries the JSON body the worker received.
fn echoed(wav: &[u8]) -> Value {
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    serde_json::from_slice(&wav[44..]).unwrap()
}

fn error(body: &[u8]) -> Value {
    serde_json::from_slice::<Value>(body).unwrap()["error"].clone()
}

fn png() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR")
}

#[tokio::test]
async fn acestep_runs_the_instrumental_and_vocal_job_pipelines() {
    let server = server(
        "acestep",
        "audio/generations",
        json!(["--models", "{model}", "--port", "{port}", "--keep-loaded"]),
    )
    .await;
    let (status, content_type, body) = post(
        &server,
        "/v1/audio/generations",
        json!({"model": "fixture.one", "prompt": "lofi beat", "duration": 12, "seed": 9, "steps": 8}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(content_type, "audio/wav");
    assert_eq!(
        echoed(&body),
        json!({"caption": "lofi beat", "synth_model": "", "output_format": "wav24", "duration": 12.0, "seed": 9, "inference_steps": 8, "lyrics": "[Instrumental]"})
    );
    let (status, _, body) = post(
        &server,
        "/api/v1/audio/generations",
        json!({"model": "fixture.one", "prompt": "folk", "lyrics": "[verse]\nla la", "vocal_language": "fr"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let synth = echoed(&body);
    assert_eq!(
        synth["audio_codes"], "1,2,3",
        "the LM job's plan feeds synth"
    );
    assert_eq!(synth["lm_mode"], "generate");
    assert_eq!(synth["vocal_language"], "fr");
    let (status, _, body) = post(
        &server,
        "/v1/audio/generations",
        json!({"model": "fixture.one", "prompt": "fail"}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        error(&body)["message"],
        "synth job ended with status 'failed'"
    );
}

#[tokio::test]
async fn sound_effect_workers_receive_their_native_fields_and_a_private_tmpdir() {
    let server = server(
        "thinksound",
        "audio/generations",
        json!(["--dir", "{model}", "--port", "{port}"]),
    )
    .await;
    let (status, _, body) = post(
        &server,
        "/v0/audio/generations",
        json!({"model": "fixture.one", "prompt": "glass", "duration": 4, "cfg": 5, "seed": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let received = echoed(&body);
    assert_eq!(received["path"], "/generate");
    assert_eq!(received["caption"], "glass");
    assert_eq!(received["description"], "glass");
    assert_eq!(received["duration"], 4.0);
    assert_eq!(received["cfg"], 5.0);
    assert!(
        received["tmpdir"]
            .as_str()
            .is_some_and(|tmpdir| tmpdir.contains("juiced-generation-") && tmpdir.ends_with("/tmp")),
        "{received}"
    );

    let moss = self::server(
        "openmoss",
        "audio/generations",
        json!(["--model", "{model}", "--port", "{port}", "--no-webui"]),
    )
    .await;
    let (status, _, body) = post(
        &moss,
        "/v1/audio/generations",
        json!({"model": "fixture.one", "prompt": "door creak", "seconds": 3, "cfg_scale": 2, "negative_prompt": "music"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let received = echoed(&body);
    assert_eq!(received["path"], "/sfx");
    assert_eq!(received["seconds"], 3.0);
    assert_eq!(received["cfg_scale"], 2.0);
    assert_eq!(received["negative_prompt"], "music");
    assert_eq!(received["response_format"], "wav");
}

#[tokio::test]
async fn audio_generation_rejects_invalid_requests_before_loading() {
    let server = Server::configured(true, |config| {
        worker(
            config,
            0,
            "fixture.one",
            "thinksound",
            "audio/generations",
            json!(["--dir", "{model}", "--port", "{port}"]),
        )
    })
    .await;
    for (body, status, code) in [
        (
            json!({"prompt": "x"}),
            StatusCode::BAD_REQUEST,
            "invalid_generation_request",
        ),
        (
            json!({"model": "fixture.one"}),
            StatusCode::BAD_REQUEST,
            "invalid_generation_request",
        ),
        (
            json!({"model": "fixture.one", "prompt": "x", "response_format": "mp3"}),
            StatusCode::BAD_REQUEST,
            "invalid_generation_request",
        ),
        (
            json!({"model": "fixture.two", "prompt": "x"}),
            StatusCode::BAD_REQUEST,
            "model_not_applicable",
        ),
        (
            json!({"model": "missing", "prompt": "x"}),
            StatusCode::NOT_FOUND,
            "unknown_model",
        ),
    ] {
        let (actual, _, reply) = post(&server, "/v1/audio/generations", body).await;
        assert_eq!(actual, status, "{}", String::from_utf8_lossy(&reply));
        assert_eq!(error(&reply)["code"], code);
    }
    let (status, _) = server
        .json(Method::GET, "/v1/audio/generations", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let health = server
        .json(Method::GET, "/v1/health", None, Some(CLIENT_KEY))
        .await
        .1;
    assert!(
        health["all_models_loaded"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model["model_name"] != "fixture.one"),
        "{health}"
    );
}

#[tokio::test]
async fn trellis_receives_the_decoded_image_and_returns_glb() {
    let server = server(
        "trellis",
        "3d/generations",
        json!(["--models", "{model}", "--port", "{port}", "--res", "512"]),
    )
    .await;
    let (status, content_type, body) = post(
        &server,
        "/v1/3d/generations",
        json!({"model": "fixture.one", "image": format!("data:image/png;base64,{}", png()), "resolution": 1024, "bg_removal": "birefnet", "uv": "box", "seed": 4}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(content_type, "model/gltf-binary");
    assert_eq!(&body[..4], b"glTF");
    let length = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;
    let chunk: Value = serde_json::from_slice(&body[20..20 + length]).unwrap();
    let fields = &chunk["fixture"];
    assert_eq!(
        fields["image"],
        json!({"file": "input.png", "content_type": "image/png", "bytes": 16})
    );
    assert_eq!(fields["resolution"], "1024");
    assert_eq!(fields["bg_removal"], "birefnet");
    assert_eq!(fields["uv"], "box");
    assert_eq!(fields["seed"], "4");
    assert!(
        fields["tmpdir"]
            .as_str()
            .is_some_and(|tmpdir| tmpdir.contains("juiced-generation-"))
    );
    for (body, message) in [
        (
            json!({"model": "fixture.one", "image": png(), "resolution": 2048}),
            "'resolution' must be 512, 1024, or 1536",
        ),
        (
            json!({"model": "fixture.one", "image": "AAAAAAAAAAAAAAAA"}),
            "'image' is not a supported format (expected PNG, JPEG, BMP, or GIF)",
        ),
    ] {
        let (status, _, reply) = post(&server, "/v1/3d/generations", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error(&reply)["message"], message);
    }
    let (status, _, reply) = post(
        &server,
        "/v1/audio/generations",
        json!({"model": "fixture.one", "prompt": "x"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error(&reply)["code"], "model_not_applicable");
}

#[tokio::test]
async fn openmoss_speech_and_thenoise_images_use_their_native_routes() {
    let server = Server::configured(true, |config| {
        worker(
            config,
            0,
            "fixture.one",
            "openmoss",
            "audio/speech",
            json!(["--model", "{model}", "--port", "{port}", "--no-webui"]),
        );
        worker(
            config,
            1,
            "fixture.two",
            "thenoise",
            "images/generations",
            json!(["serve", "--dit", "{model}", "--port", "{port}"]),
        );
    })
    .await;
    let (status, content_type, body) = post(
        &server,
        "/v1/audio/speech",
        json!({"model": "fixture.one", "input": "hello", "voice": "A calm narrator with a deep voice", "instructions": "slowly"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(content_type, "audio/wav");
    let received = echoed(&body);
    assert_eq!(received["voice"], "A calm narrator with a deep voice");
    assert_eq!(received["instruction"], "slowly");
    assert_eq!(received["response_format"], "wav");
    let (status, _, body) = post(
        &server,
        "/v1/audio/speech",
        json!({"model": "fixture.one", "input": "hello", "response_format": "mp3"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error(&body)["code"], "unsupported_parameter");

    let (status, content_type, body) = post(
        &server,
        "/v1/images/generations",
        json!({"model": "fixture.two", "prompt": "a lighthouse", "size": "768x512", "steps": 8, "seed": 3}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(content_type, "application/json");
    let reply: Value = serde_json::from_slice(&body).unwrap();
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD
        .decode(reply["data"][0]["b64_json"].as_str().unwrap())
        .unwrap();
    let received: Value = serde_json::from_slice(&png[8..]).unwrap();
    assert_eq!(
        received,
        json!({"prompt": "a lighthouse", "out": "json", "width": 768, "height": 512, "steps": 8, "seed": 3})
    );
}
