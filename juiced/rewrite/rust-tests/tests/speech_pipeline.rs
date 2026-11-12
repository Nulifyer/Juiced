mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path, time::Duration};
use support::{
    PREFIXES, Server,
    speech::{self, Part},
};

async fn load(server: &Server, model: &str) -> Value {
    let (status, loaded) = server.post("load", json!({"model":model})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    loaded
}
fn exited(pid: &Value) {
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new(&format!("/proc/{}", pid.as_u64().unwrap())).exists(),
        "owned speech child survived exit acknowledgement"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}
async fn retired(server: &Server, previous: &Value, remaining_bytes: u64) {
    server
        .wait_for(async || {
            let model = server.model("fixture.one").await;
            let admission = server.get("juiced/admission").await;
            model["loaded"] == false
                && model["pid"].is_null()
                && admission["resources"]["active_requests"] == 0
                && admission["resources"]["reserved_bytes"] == remaining_bytes
                && admission["resources"]["power"]["active"] == 0
                && admission["scheduler"]["active"] == 0
        })
        .await;
    exited(&previous["pid"]);
    assert_eq!(
        server.model("fixture.one").await["cleanup_retry_available"],
        false
    );
    assert_eq!(
        server.model("fixture.one").await["policy"],
        previous["policy"]
    );
    assert_eq!(
        server.model("fixture.one").await["revision"],
        previous["revision"]
    );
}
async fn chat(server: &Server, previous: Option<&Value>) -> Value {
    let (status, result) = server
        .post(
            "completions",
            json!({"model":"fixture.two","prompt":"Unrelated chat pipeline"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["model"], "fixture.two");
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let model = server.model("fixture.two").await;
    if let Some(previous) = previous {
        assert_eq!(model["pid"], previous["pid"]);
        assert_eq!(
            model["process_lifecycle"]["generation"],
            previous["process_lifecycle"]["generation"]
        );
    }
    model
}

#[tokio::test]
async fn private_fixture_physical_speech_work_survives_dropped_http_future_until_explicit_release()
{
    use tokio::io::AsyncWriteExt;
    let mut release = None;
    let server = speech::server(|config| {
        let path = Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap()
            .join("physical-speech-release");
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--speech-physical-release-file"), json!(path)]);
        release = Some(path);
    })
    .await;
    let before = load(&server, "fixture.one").await;
    let events =
        std::fs::read_to_string(server.directory.path().join("fixture.one.events")).unwrap();
    let startup = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|value| value["id"] == "worker-startup" && value["pid"] == before["pid"])
        .unwrap();
    let port = u16::try_from(startup["port"].as_u64().unwrap()).unwrap();
    assert_ne!(port, 0);
    let audio = speech::audio();
    let (content_type, body) = speech::multipart(&[
        Part::File("file", "probe.wav", "audio/wav", &audio),
        Part::Text("response_format", "json"),
        Part::Text("temperature", "0"),
        Part::Text("language", "auto"),
        Part::Text("prompt", ""),
    ]);
    let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    socket.write_all(format!("POST /inference HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    server
        .wait_for(async || server.event("fixture.one", "speech-physical", "started"))
        .await;
    drop(socket);
    server
        .wait_for(async || server.event("fixture.one", "speech-request", "cancelled"))
        .await;
    assert!(!server.event("fixture.one", "speech-physical", "settled"));
    assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
    std::fs::write(release.unwrap(), b"release detached physical fixture work").unwrap();
    server
        .wait_for(async || server.event("fixture.one", "speech-physical", "settled"))
        .await;
    assert!(!server.event("fixture.one", "speech-physical", "timed-out"));
    assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
}

#[tokio::test]
async fn transcription_works_under_all_native_prefixes_with_auth_defaults_exact_audio_and_no_key_passthrough()
 {
    let server = speech::server(|_| {}).await;
    let audio = speech::audio();
    let parts = [
        Part::Text("model", "fixture.one"),
        Part::File("file", "probe.wav", "application/octet-stream", &audio),
    ];
    let mut prior: Option<Value> = None;
    for prefix in PREFIXES {
        for key in [None, Some("invalid-speech-key")] {
            let response = speech::send(
                &server,
                &format!("{prefix}/audio/transcriptions"),
                &parts,
                key,
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert!(response.headers().contains_key("www-authenticate"));
        }
        let response = speech::send(
            &server,
            &format!("{prefix}/audio/transcriptions"),
            &parts,
            server.read_key(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("application/json")
        );
        let value: Value = response.json().await.unwrap();
        assert!(value["text"].is_string());
        assert_eq!(
            value["received_file"]["sha256"],
            format!("{:x}", Sha256::digest(&audio))
        );
        assert_eq!(value["received_file"]["bytes"], audio.len());
        assert_eq!(value["received_file"]["filename"], "probe.wav");
        assert_eq!(value["received_file"]["content_type"], "audio/wav");
        assert_eq!(
            value["received_fields"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            ["response_format", "temperature", "language", "prompt"]
                .into_iter()
                .collect()
        );
        assert_eq!(value["received_fields"]["response_format"], "json");
        assert_eq!(
            value["received_fields"]["temperature"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap(),
            0.0
        );
        assert_eq!(value["received_fields"]["language"], "auto");
        assert_eq!(value["received_fields"]["prompt"], "");
        assert_eq!(value["received_model"], false);
        assert_eq!(value["received_authorization"], false);
        assert_eq!(value["manager_key_present"], false);
        let ready = server.model("fixture.one").await;
        if let Some(prior) = &prior {
            assert_eq!(ready["pid"], prior["pid"]);
            assert_eq!(
                ready["process_lifecycle"]["generation"],
                prior["process_lifecycle"]["generation"]
            );
        }
        prior = Some(ready);
    }
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let records = server.get("juiced/requests").await;
    assert_eq!(records["data"].as_array().unwrap().len(), 4);
    assert_eq!(records["contains_content"], false);
    for record in records["data"].as_array().unwrap() {
        assert_eq!(record["operation"], "audio/transcriptions");
        assert_eq!(record["outcome"], "completed");
    }
    assert!(!records.to_string().contains("private-audio-marker"));
    assert!(!records.to_string().contains("Fixture speech"));
}

#[tokio::test]
async fn explicit_transcription_fields_formats_binary_mime_and_safe_filename_reach_native_worker() {
    let server = speech::server(|_| {}).await;
    let audio = speech::audio();
    let prompt = "private-speech-prompt-do-not-log λ";
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::File("file", "/private/path/probe.wav", "text/plain", &audio),
            Part::Text("response_format", "verbose_json"),
            Part::Text("temperature", "0.25"),
            Part::Text("language", "DE"),
            Part::Text("prompt", prompt),
            Part::Text("model", "fixture.one"),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["received_fields"]["prompt"], prompt);
    assert_eq!(value["received_fields"]["response_format"], "verbose_json");
    assert_eq!(
        value["received_fields"]["temperature"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        0.25
    );
    assert_eq!(value["received_fields"]["language"], "de");
    assert_eq!(value["received_file"]["filename"], "probe.wav");
    for (format, mime, expected) in [
        ("text", "text/plain", "Fixture speech λ\n"),
        (
            "srt",
            "text/plain",
            "1\n00:00:00,000 --> 00:00:01,000\nFixture speech λ\n",
        ),
        (
            "vtt",
            "text/plain",
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nFixture speech λ\n",
        ),
    ] {
        let response = speech::send(
            &server,
            "/v1/audio/transcriptions",
            &[
                Part::Text("model", "fixture.one"),
                Part::Text("response_format", format),
                Part::File("file", "probe.wav", "audio/wav", &audio),
            ],
            server.read_key(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with(mime)
        );
        assert_eq!(response.text().await.unwrap(), expected);
    }
    let binary = [0, 1, 2, 255, 254, 0, 128];
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::Text("model", "fixture.one"),
            Part::File("file", "probe.bin", "audio/wav", &binary),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value["received_file"]["content_type"],
        "application/octet-stream"
    );
    assert_eq!(
        value["received_file"]["sha256"],
        format!("{:x}", Sha256::digest(binary))
    );
    let medium = vec![0x5a; 3 * 1024 * 1024];
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::Text("model", "fixture.one"),
            Part::File("file", "medium.bin", "application/octet-stream", &medium),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["received_file"]["bytes"], medium.len());
    assert_eq!(
        value["received_file"]["sha256"],
        format!("{:x}", Sha256::digest(&medium))
    );
    let records = server.get("juiced/requests").await.to_string();
    assert!(!records.contains(prompt));
    assert!(!records.contains("private-audio-marker"));
    assert!(!records.contains("Fixture speech"));
}

#[tokio::test]
async fn malformed_duplicate_unknown_and_oversized_transcription_inputs_fail_before_loading() {
    let server = speech::server(|_| {}).await;
    let audio = speech::audio();
    let disk = std::fs::read(&server.config_path).unwrap();
    for parts in [
        vec![Part::Text("model", "fixture.one")],
        vec![Part::File("file", "probe.wav", "audio/wav", &audio)],
        vec![
            Part::Text("model", ""),
            Part::File("file", "probe.wav", "audio/wav", &audio),
        ],
        vec![
            Part::Text("model", "fixture.one"),
            Part::File("file", "empty.wav", "audio/wav", &[]),
        ],
        vec![
            Part::Text("model", "fixture.one"),
            Part::Text("model", "fixture.one"),
            Part::File("file", "probe.wav", "audio/wav", &audio),
        ],
        vec![
            Part::Text("model", "fixture.one"),
            Part::File("file", "one.wav", "audio/wav", &audio),
            Part::File("file", "two.wav", "audio/wav", &audio),
        ],
    ] {
        let response = speech::send(
            &server,
            "/v1/audio/transcriptions",
            &parts,
            server.read_key(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.json::<Value>().await.unwrap()["error"]["message"].is_string());
    }
    for (field, value) in [
        ("stream", "false"),
        ("timestamp_granularities[]", "word"),
        ("unknown", "value"),
        ("response_format", "xml"),
        ("temperature", "NaN"),
        ("temperature", "inf"),
        ("temperature", "-1"),
        ("temperature", "2"),
        ("language", "en-US"),
        ("language", "d\ne"),
    ] {
        let response = speech::send(
            &server,
            "/v1/audio/transcriptions",
            &[
                Part::Text("model", "fixture.one"),
                Part::File("file", "probe.wav", "audio/wav", &audio),
                Part::Text(field, value),
            ],
            server.read_key(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{field}");
    }
    for field in ["response_format", "temperature", "language", "prompt"] {
        let value = match field {
            "response_format" => "json",
            "temperature" => "0",
            "language" => "auto",
            _ => "",
        };
        let response = speech::send(
            &server,
            "/v1/audio/transcriptions",
            &[
                Part::Text("model", "fixture.one"),
                Part::File("file", "probe.wav", "audio/wav", &audio),
                Part::Text(field, value),
                Part::Text(field, value),
            ],
            server.read_key(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "duplicate {field}"
        );
    }
    for (field, value) in [
        ("prompt", "x".repeat(16 * 1024 + 1)),
        ("model", "x".repeat(257)),
        ("language", "x".repeat(33)),
        ("temperature", "0".repeat(33)),
        ("response_format", "x".repeat(33)),
    ] {
        let mut parts = vec![Part::File("file", "probe.wav", "audio/wav", &audio)];
        if field != "model" {
            parts.push(Part::Text("model", "fixture.one"));
        }
        parts.push(Part::Text(field, &value));
        assert_eq!(
            speech::send(
                &server,
                "/v1/audio/transcriptions",
                &parts,
                server.read_key()
            )
            .await
            .status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "{field}"
        );
    }
    let oversized = vec![0; 8 * 1024 * 1024];
    assert_eq!(
        speech::send(
            &server,
            "/v1/audio/transcriptions",
            &[
                Part::Text("model", "fixture.one"),
                Part::File("file", "large.wav", "audio/wav", &oversized)
            ],
            server.read_key()
        )
        .await
        .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let (content_type, mut body) = speech::multipart(&[
        Part::Text("model", "fixture.one"),
        Part::File("file", "probe.wav", "audio/wav", &audio),
    ]);
    body.truncate(body.len() - 45);
    let response = server
        .client
        .post(format!("{}/v1/audio/transcriptions", server.base))
        .bearer_auth(server.read_key().unwrap())
        .header("content-type", content_type)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    for content_type in ["application/json", "multipart/form-data"] {
        let response = server
            .client
            .post(format!("{}/v1/audio/transcriptions", server.base))
            .bearer_auth(server.read_key().unwrap())
            .header("content-type", content_type)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "invalid_multipart"
        );
    }
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(server.get("juiced/requests").await["data"], json!([]));
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert!(!server.event("fixture.one", "speech-request", "started"));
}

#[tokio::test]
async fn fully_framed_transcription_errors_are_redacted_and_preserve_the_physical_worker() {
    for mode in [
        "invalid-json",
        "missing-text",
        "error-object",
        "invalid-utf8",
    ] {
        let server = speech::server(|config| {
            config["models"][0]["args"]
                .as_array_mut()
                .unwrap()
                .extend([json!("--speech-response-mode"), json!(mode)])
        })
        .await;
        let before = load(&server, "fixture.one").await;
        let audio = speech::audio();
        let format = if mode == "invalid-utf8" {
            "text"
        } else {
            "json"
        };
        let response = speech::send(
            &server,
            "/v1/audio/transcriptions",
            &[
                Part::Text("model", "fixture.one"),
                Part::Text("response_format", format),
                Part::File("file", "probe.wav", "audio/wav", &audio),
            ],
            server.read_key(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "{mode}");
        let error: Value = response.json().await.unwrap();
        assert!(!error.to_string().contains("private worker error"));
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
        assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
        assert_eq!(
            server.model("fixture.one").await["process_lifecycle"]["generation"],
            before["process_lifecycle"]["generation"]
        );
    }
    let server = speech::server(|config| {
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--speech-status"), json!("429")])
    })
    .await;
    let before = load(&server, "fixture.one").await;
    let audio = speech::audio();
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::Text("model", "fixture.one"),
            Part::File("file", "probe.wav", "audio/wav", &audio),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "transcription_rejected"
    );
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
}

#[tokio::test]
async fn transcription_timeout_and_unframed_response_limit_retire_exact_workers_without_restarting_chat()
 {
    for timeout in [true, false] {
        let mut release = None;
        let server = speech::server(|config| {
            let path = Path::new(config["models"][0]["model_file"].as_str().unwrap())
                .parent()
                .unwrap()
                .join("uncertain-physical-release");
            if !timeout {
                std::fs::write(&path, b"physical work may finish before oversized reply").unwrap();
            }
            config["models"][0]["args"]
                .as_array_mut()
                .unwrap()
                .extend([json!("--speech-physical-release-file"), json!(path)]);
            if timeout {
                config["request_timeout_ms"] = json!(100);
            } else {
                config["models"][0]["args"]
                    .as_array_mut()
                    .unwrap()
                    .extend([json!("--speech-response-mode"), json!("oversized")]);
            }
            release = Some(path);
        })
        .await;
        let unrelated = chat(&server, None).await;
        let before = load(&server, "fixture.one").await;
        let audio = speech::audio();
        let parts = [
            Part::Text("model", "fixture.one"),
            Part::File("file", "probe.wav", "audio/wav", &audio),
        ];
        let response = speech::send(
            &server,
            "/v1/audio/transcriptions",
            &parts,
            server.read_key(),
        )
        .await;
        assert_eq!(
            response.status(),
            if timeout {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            }
        );
        let error: Value = response.json().await.unwrap();
        assert_eq!(
            error["error"]["code"],
            if timeout {
                "upstream_timeout"
            } else {
                "transcription_response_limit"
            }
        );
        retired(&server, &before, 64).await;
        assert_eq!(
            server.event_count("fixture.one", "speech-physical", "started"),
            1
        );
        assert_eq!(
            server.event_count("fixture.one", "speech-physical", "settled"),
            usize::from(!timeout)
        );
        assert!(!server.event("fixture.one", "speech-physical", "timed-out"));
        chat(&server, Some(&unrelated)).await;
        std::fs::write(release.unwrap(), b"allow new generation to complete").unwrap();
        let after = load(&server, "fixture.one").await;
        assert_ne!(after["pid"], before["pid"]);
        assert!(
            after["process_lifecycle"]["generation"].as_u64().unwrap()
                > before["process_lifecycle"]["generation"].as_u64().unwrap()
        );
        assert_eq!(after["policy"], before["policy"]);
        assert_eq!(after["revision"], before["revision"]);
        if timeout {
            let response = speech::send(
                &server,
                "/api/v0/audio/transcriptions",
                &parts,
                server.read_key(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.json::<Value>().await.unwrap()["text"].is_string());
            assert_eq!(server.model("fixture.one").await["pid"], after["pid"]);
        }
        let (status, unloaded) = server.post("unload", json!({"model":"fixture.one"})).await;
        assert_eq!(status, StatusCode::OK, "{unloaded}");
        exited(&after["pid"]);
        chat(&server, Some(&unrelated)).await;
    }
}

#[tokio::test]
async fn transcription_disconnect_retires_physical_generation_and_queued_work_obeys_admission() {
    async fn wait_for(label: &str, mut condition: impl AsyncFnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition().await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label} did not settle within three seconds"));
    }
    let mut release_file = None;
    let server = speech::server(|config| {
        let path = std::path::Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap()
            .join("speech-release");
        assert!(!path.exists());
        config["admission"] = json!({"interactive_wait_ms":100});
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--speech-physical-release-file"), json!(path)]);
        release_file = Some(path);
    })
    .await;
    let release_file = release_file.unwrap();
    let unrelated = chat(&server, None).await;
    let audio = speech::audio();
    let parts = [
        Part::Text("model", "fixture.one"),
        Part::File("file", "probe.wav", "audio/wav", &audio),
    ];
    let held = speech::raw(&server, &parts).await;
    wait_for("initial native transcription start", async || {
        server.event("fixture.one", "speech-physical", "started")
    })
    .await;
    let before = server.model("fixture.one").await;
    let pid = before["pid"].clone();
    assert_eq!(
        server.get("juiced/state").await["resources"]["active_requests"],
        1
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["power"]["active"],
        1
    );
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &parts,
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "queue_timeout"
    );
    assert_eq!(
        server.event_count("fixture.one", "speech-request", "started"),
        1
    );
    drop(held);
    retired(&server, &before, 64).await;
    assert_eq!(
        server.get("juiced/admission").await["scheduler"]["pending"],
        0
    );
    assert!(!server.event("fixture.one", "speech-physical", "settled"));
    chat(&server, Some(&unrelated)).await;
    std::fs::write(release_file, b"release fixture speech").unwrap();
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &parts,
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    let after = server.model("fixture.one").await;
    assert_ne!(after["pid"], pid);
    assert!(
        after["process_lifecycle"]["generation"].as_u64().unwrap()
            > before["process_lifecycle"]["generation"].as_u64().unwrap()
    );
    assert_eq!(after["policy"], before["policy"]);
    assert_eq!(after["revision"], before["revision"]);
    chat(&server, Some(&unrelated)).await;
}

#[tokio::test]
async fn transcription_registration_requires_selected_provider_single_slot_and_no_typed_llm_options()
 {
    let server = speech::server(|_| {}).await;
    let disk = std::fs::read(&server.config_path).unwrap();
    for change in [
        json!({"provider":"external-http"}),
        json!({"max_inflight":2}),
        json!({"operations":["audio/transcriptions","completions"]}),
        json!({"options":{"threads":2}}),
        json!({"args":["--port","{port}"]}),
    ] {
        let mut spec = server.config["models"][0].clone();
        spec["id"] = json!("invalid.speech");
        spec["policy"] = json!("disabled");
        spec["revision"] = json!(0);
        for (key, value) in change.as_object().unwrap() {
            spec[key] = value.clone();
        }
        assert_eq!(
            server.post("models/register", spec).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    let (status, error) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":0,"options":{"threads":2}}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(error["error"]["code"], "typed_options_unsupported");
    let mut spec = server.config["models"][0].clone();
    spec["id"] = json!("registered.speech");
    spec["policy"] = json!("disabled");
    spec["revision"] = json!(0);
    let (status, registered) = server.post("models/register", spec).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(registered["operations"], json!(["audio/transcriptions"]));
    assert_eq!(registered["max_inflight"], 1);
    assert_eq!(registered["loaded"], false);
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"registered.speech","expected_revision":registered["revision"]})
            )
            .await
            .1["weights_retained"],
        true
    );
}

#[tokio::test]
async fn successful_transcription_does_not_bind_a_persistent_completion_cache_session() {
    let server = speech::server(|config| {
        config["cache_enabled"] = json!(true);
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--slot-save-path"), json!("{cache}")]);
    })
    .await;
    let audio = speech::audio();
    let (content_type, body) = speech::multipart(&[
        Part::Text("model", "fixture.one"),
        Part::File("file", "probe.wav", "audio/wav", &audio),
    ]);
    let response = server
        .client
        .post(format!("{}/v1/audio/transcriptions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", "speech-session")
        .header("content-type", content_type)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let before = server
        .json(
            reqwest::Method::GET,
            "/v1/juiced/cache",
            None,
            server.admin_key(),
        )
        .await
        .1;
    let(status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"speech-session","expected_generation":before["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "cache_context_mismatch");
    assert_eq!(
        server
            .json(
                reqwest::Method::GET,
                "/v1/juiced/cache",
                None,
                server.admin_key()
            )
            .await
            .1,
        before
    );
    assert!(!server.event("fixture.one", "slot-save", "started"));
}

#[tokio::test]
async fn selected_bundled_speech_worker_binds_redacted_digest_and_refuses_changed_or_extra_files_before_loading()
 {
    let mut executable = None;
    let mut bundle_root = None;
    let server=speech::server(|config|{
        let model=&mut config["models"][0];let source=std::path::PathBuf::from(model["command"].as_str().unwrap());
        let root=std::path::Path::new(model["model_file"].as_str().unwrap()).parent().unwrap().join("owned speech bundle");std::fs::create_dir(&root).unwrap();
        let name=format!("juiced-test-worker{}",std::env::consts::EXE_SUFFIX);let path=root.join(&name);std::fs::copy(&source,&path).unwrap();
        let sha256=format!("{:x}",Sha256::digest(std::fs::read(&path).unwrap()));
        model["command"]=json!(path);model["runtime_bundle"]=json!({"schema_version":1,"implementation_id":"whisper-cpp","root":root,"entrypoint":name,"library_directories":[],"files":[{"path":name,"sha256":sha256,"role":"executable"}]});executable=Some(path);bundle_root=Some(root);
    }).await;
    let executable = executable.unwrap();
    let root = bundle_root.unwrap();
    let audio = speech::audio();
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::Text("model", "fixture.one"),
            Part::File("file", "probe.wav", "audio/wav", &audio),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    let ready = server.model("fixture.one").await;
    assert_eq!(ready["implementation_id"], "whisper-cpp");
    let summary = &ready["runtime_identity"];
    assert_eq!(summary["bundle_digest"].as_str().unwrap().len(), 64);
    let serialized = serde_json::to_string(&ready).unwrap();
    assert!(!serialized.contains(root.to_str().unwrap()));
    assert!(!serialized.contains(executable.to_str().unwrap()));
    assert!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0
            .is_success()
    );
    let extra = root.join("unexpected-resource");
    std::fs::write(&extra, b"unlisted fixture file").unwrap();
    let (status, error) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error["error"]["code"], "worker_identity");
    assert_eq!(server.model("fixture.one").await["pid"], Value::Null);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    std::fs::remove_file(extra).unwrap();
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&executable)
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 1;
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let (status, error) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error["error"]["code"], "worker_identity");
    assert_eq!(server.model("fixture.one").await["pid"], Value::Null);
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn slow_unread_multipart_reserves_bounded_ingress_without_blocking_auth_health_or_operator_controls()
 {
    use tokio::io::AsyncWriteExt;
    let server =
        speech::server(|config| config["admission"] = json!({"ingress_body_bytes":8*1024*1024}))
            .await;
    let disk = std::fs::read(&server.config_path).unwrap();
    let history = server.get("juiced/requests").await;
    let cache = server.get("juiced/cache").await;
    let mut slow = tokio::net::TcpStream::connect(server.base.trim_start_matches("http://"))
        .await
        .unwrap();
    slow.write_all(format!("POST /api/v1/audio/transcriptions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: multipart/form-data; boundary=slow-speech-ingress\r\nContent-Length: 4096\r\n\r\n",server.read_key().unwrap()).as_bytes()).await.unwrap();
    server
        .wait_for(async || {
            server.get("juiced/admission").await["ingress"]["reserved_bytes"] == 8 * 1024 * 1024
        })
        .await;
    let held = server.get("juiced/admission").await;
    assert_eq!(held["ingress"]["capacity_bytes"], 8 * 1024 * 1024);
    assert_eq!(held["ingress"]["available_bytes"], 0);
    assert_eq!(held["scheduler"]["active"], 0);
    assert_eq!(held["scheduler"]["pending"], 0);
    let audio = speech::audio();
    let parts = [
        Part::Text("model", "fixture.one"),
        Part::File("file", "probe.wav", "audio/wav", &audio),
    ];
    for key in [None, Some("invalid-ingress-key")] {
        assert_eq!(
            speech::send(&server, "/v1/audio/transcriptions", &parts, key)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &parts,
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "ingress_capacity"
    );
    let (status, error) = server
        .post("completions", json!({"model":"fixture.two"}))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "ingress_capacity");
    assert_eq!(server.get("health").await["status"], "ok");
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":true}))
            .await
            .1["paused"],
        true
    );
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":false}))
            .await
            .1["paused"],
        false
    );
    assert_eq!(server.model("fixture.one").await["pid"], Value::Null);
    assert_eq!(server.model("fixture.two").await["pid"], Value::Null);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(server.get("juiced/requests").await, history);
    assert_eq!(server.get("juiced/cache").await, cache);
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    drop(slow);
    server
        .wait_for(async || server.get("juiced/admission").await["ingress"]["reserved_bytes"] == 0)
        .await;
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &parts,
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    server
        .wait_for(async || server.get("juiced/admission").await["ingress"]["reserved_bytes"] == 0)
        .await;
    assert_eq!(
        server.get("juiced/requests").await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
