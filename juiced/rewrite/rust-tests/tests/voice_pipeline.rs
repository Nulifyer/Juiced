mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use support::{
    ADMIN_KEY, CLIENT_KEY, PREFIXES, Server, speech,
    voice::{self, Wyoming},
};

async fn voice_server() -> Server {
    speech::server(|_| {}).await
}

fn address(view: &Value) -> String {
    assert_eq!(view["listener"]["listening"], true, "{view}");
    view["listener"]["address"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn wyoming_listener_restarts_on_change_and_serves_describe_transcribe_and_synthesize() {
    let mut server = voice_server().await;
    let initial = server.get("juiced/wyoming").await;
    assert_eq!(initial["schema_version"], 1);
    assert_eq!(
        initial["settings"],
        json!({"enabled": false, "listen": "0.0.0.0:10300", "stt_model": null, "tts_model": null, "tts_voice": null})
    );
    assert_eq!(initial["listener"]["listening"], false);
    assert_eq!(initial["resolved"]["stt_models"], json!(["fixture.one"]));
    assert_eq!(initial["resolved"]["tts_model"], Value::Null);
    assert_eq!(initial["active_connections"], 0);

    for (body, status, code) in [
        (
            json!({"settings": {"enabled": true, "listen": "127.0.0.1:0"}, "expected_generation": 999}),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
        (
            json!({"settings": {"enabled": true, "listen": "localhost"}, "expected_generation": initial["configuration_generation"]}),
            StatusCode::BAD_REQUEST,
            "invalid_wyoming_settings",
        ),
        (
            json!({"settings": {"enabled": true, "tts_voice": "../x"}, "expected_generation": initial["configuration_generation"]}),
            StatusCode::BAD_REQUEST,
            "invalid_wyoming_settings",
        ),
    ] {
        let (actual, error) = server.post("juiced/wyoming", body).await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["error"]["code"], code);
    }
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/juiced/wyoming",
            Some(json!({"settings": {}, "expected_generation": 0})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let enabled = voice::wyoming(&server, json!({"enabled": true, "listen": "127.0.0.1:0"})).await;
    assert_eq!(
        enabled["configuration_generation"],
        initial["configuration_generation"].as_u64().unwrap() + 1
    );
    let first = address(&enabled);
    let mut client = Wyoming::connect(&first).await;
    client.send("describe", json!({}), &[]).await;
    let info = client.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(info.kind, "info");
    assert_eq!(info.data["asr"][0]["installed"], true);
    assert_eq!(info.data["asr"][0]["models"][0]["name"], "fixture.one");
    assert_eq!(info.data["asr"][0]["models"][0]["installed"], true);
    assert_eq!(info.data["tts"], json!([]));

    let stereo: Vec<u8> = voice::tone(48_000, 500, 0.3)
        .into_iter()
        .flat_map(|sample| [sample, sample])
        .flat_map(i16::to_le_bytes)
        .collect();
    let transcript = client.transcribe("en-US", 48_000, 2, 2, &stereo).await;
    assert_eq!(transcript.kind, "transcript", "{:?}", transcript.data);
    assert_eq!(transcript.data["text"], "Fixture speech λ");
    assert!(server.event("fixture.one", "speech-request", "completed"));

    let (refused, _, _) = client
        .synthesize(json!({"text": "Hello from Juiced.", "voice": {"name": "am_adam"}}))
        .await;
    assert_eq!(refused.kind, "error");
    assert_eq!(refused.data["code"], "wyoming_model_unavailable");

    client
        .raw(b"{\"type\":\"x\",\"payload_length\":-1}\n")
        .await;
    let error = client.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(error.kind, "error");
    assert_eq!(error.data["code"], "malformed_event");
    assert!(client.next(Duration::from_secs(5)).await.is_none());

    let mut stray = Wyoming::connect(&first).await;
    stray.send("audio-stop", json!({}), &[]).await;
    assert_eq!(
        stray.next(Duration::from_secs(5)).await.unwrap().data["code"],
        "audio_not_started"
    );
    drop(stray);

    let view = server.get("juiced/wyoming").await;
    assert!(view["total_connections"].as_u64().unwrap() >= 2);
    assert_eq!(view["last_event"]["type"], "synthesize");
    assert_eq!(view["last_event"]["outcome"], "wyoming_model_unavailable");
    assert_eq!(view["last_event"]["model"], Value::Null);

    let pinned = voice::wyoming(
        &server,
        json!({"enabled": true, "listen": "127.0.0.1:0", "stt_model": "fixture.disabled", "tts_model": "fixture.one", "tts_voice": "bf_emma"}),
    )
    .await;
    let second = address(&pinned);
    assert_eq!(
        pinned["resolved"]["stt_models"],
        json!(["fixture.disabled"])
    );
    assert_eq!(pinned["resolved"]["tts_voices"][0], "bf_emma");
    let mut client = Wyoming::connect(&second).await;
    let refused = client
        .transcribe(
            "de",
            16_000,
            2,
            1,
            &voice::pcm(&voice::tone(16_000, 300, 0.3)),
        )
        .await;
    assert_eq!(refused.kind, "error");
    assert_eq!(refused.data["code"], "model_disabled");
    let (refused, _, _) = client.synthesize(json!({"text": "x"})).await;
    assert_eq!(refused.data["code"], "unsupported_operation");

    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(saved["wyoming"]["tts_voice"], "bf_emma");
    server.restart().await;
    let restarted = server.get("juiced/wyoming").await;
    assert_eq!(restarted["settings"]["stt_model"], "fixture.disabled");
    assert_eq!(restarted["listener"]["listening"], true);

    let generation = restarted["configuration_generation"].clone();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied = busy.local_addr().unwrap().to_string();
    let (status, failed) = server
        .post(
            "juiced/wyoming",
            json!({"settings": {"enabled": true, "listen": occupied}, "expected_generation": generation}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{failed}");
    assert_eq!(failed["listener"]["listening"], false);
    assert_eq!(failed["listener"]["error"]["code"], "wyoming_bind_failed");
    drop(busy);
    let disabled = voice::wyoming(&server, json!({"enabled": false})).await;
    assert_eq!(disabled["listener"]["listening"], false);
    assert_eq!(disabled["listener"]["error"], Value::Null);
}

#[tokio::test]
async fn realtime_transcription_sessions_authenticate_buffer_detect_turns_and_transcribe() {
    let server = voice_server().await;
    for prefix in PREFIXES {
        let path = format!("{prefix}/realtime?intent=transcription");
        for key in [None, Some("invalid-key")] {
            let error = voice::realtime(&server, &path, key, None)
                .await
                .err()
                .unwrap();
            let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
                panic!("expected an HTTP refusal, got {error}");
            };
            assert_eq!(response.status(), 401);
        }
        let (mut socket, _) = voice::realtime(&server, &path, Some(CLIENT_KEY), None)
            .await
            .unwrap();
        let created = voice::receive(&mut socket, Duration::from_secs(5)).await;
        assert_eq!(created["type"], "transcription_session.created");
        assert_eq!(created["session"]["input_audio_format"], "pcm16");
        assert_eq!(created["session"]["turn_detection"]["type"], "server_vad");
    }
    let refused = voice::realtime(
        &server,
        "/v1/realtime?intent=conversation",
        Some(CLIENT_KEY),
        None,
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(refused, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 400)
    );
    let (status, plain) = server
        .json(Method::GET, "/v1/realtime", None, Some(CLIENT_KEY))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(plain["error"]["code"], "websocket_required");

    let (mut socket, selected) = voice::realtime(
        &server,
        "/v1/realtime?intent=transcription&model=fixture.one",
        None,
        Some(&format!("realtime, openai-insecure-api-key.{ADMIN_KEY}")),
    )
    .await
    .unwrap();
    assert_eq!(selected.as_deref(), Some("realtime"));
    let created = voice::receive(&mut socket, Duration::from_secs(5)).await;
    assert_eq!(
        created["session"]["input_audio_transcription"]["model"],
        "fixture.one"
    );

    voice::send(&mut socket, json!({"type": "transcription_session.update", "session": {"input_audio_format": "pcm16", "turn_detection": null}})).await;
    let updated = voice::receive(&mut socket, Duration::from_secs(5)).await;
    assert_eq!(updated["type"], "transcription_session.updated");
    assert_eq!(updated["session"]["turn_detection"], Value::Null);
    voice::send(
        &mut socket,
        json!({"type": "input_audio_buffer.commit", "event_id": "client-1"}),
    )
    .await;
    let empty = voice::receive(&mut socket, Duration::from_secs(5)).await;
    assert_eq!(empty["type"], "error");
    assert_eq!(empty["error"]["code"], "input_audio_buffer_commit_empty");
    assert_eq!(empty["error"]["event_id"], "client-1");
    voice::send(
        &mut socket,
        json!({"type": "session.update", "session": {"input_audio_format": "g711_ulaw"}}),
    )
    .await;
    let unsupported = voice::receive(&mut socket, Duration::from_secs(5)).await;
    assert_eq!(unsupported["error"]["code"], "unsupported_audio_format");
    voice::send(
        &mut socket,
        json!({"type": "input_audio_buffer.append", "audio": "!!"}),
    )
    .await;
    assert_eq!(
        voice::receive(&mut socket, Duration::from_secs(5)).await["error"]["code"],
        "invalid_audio"
    );

    voice::append(&mut socket, &voice::tone(24_000, 500, 0.3)).await;
    voice::send(&mut socket, json!({"type": "input_audio_buffer.commit"})).await;
    let committed = voice::receive(&mut socket, Duration::from_secs(5)).await;
    assert_eq!(committed["type"], "input_audio_buffer.committed");
    assert_eq!(committed["previous_item_id"], Value::Null);
    let item = committed["item_id"].as_str().unwrap().to_owned();
    let (completed, _) = voice::until(
        &mut socket,
        "conversation.item.input_audio_transcription.completed",
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(completed["item_id"], item);
    assert_eq!(completed["transcript"], "Fixture speech λ");

    voice::append(&mut socket, &voice::tone(24_000, 200, 0.3)).await;
    voice::send(&mut socket, json!({"type": "input_audio_buffer.clear"})).await;
    assert_eq!(
        voice::receive(&mut socket, Duration::from_secs(5)).await["type"],
        "input_audio_buffer.cleared"
    );

    voice::send(&mut socket, json!({"type": "transcription_session.update", "session": {"turn_detection": {"type": "server_vad", "threshold": 0.5, "silence_duration_ms": 200, "prefix_padding_ms": 100}}})).await;
    assert_eq!(
        voice::receive(&mut socket, Duration::from_secs(5)).await["session"]["turn_detection"]["silence_duration_ms"],
        200
    );
    let mut turn = vec![0i16; 24_000 * 3 / 10];
    turn.extend(voice::tone(24_000, 500, 0.3));
    turn.extend(vec![0i16; 24_000 * 6 / 10]);
    voice::append(&mut socket, &turn).await;
    let (started, _) = voice::until(
        &mut socket,
        "input_audio_buffer.speech_started",
        Duration::from_secs(5),
    )
    .await;
    let (stopped, _) = voice::until(
        &mut socket,
        "input_audio_buffer.speech_stopped",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(started["item_id"], stopped["item_id"]);
    let start_ms = started["audio_start_ms"].as_u64().unwrap();
    let end_ms = stopped["audio_end_ms"].as_u64().unwrap();
    assert!((850..=1000).contains(&start_ms), "{start_ms}");
    assert!((1650..=1750).contains(&end_ms), "{end_ms}");
    let (committed, _) = voice::until(
        &mut socket,
        "input_audio_buffer.committed",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(committed["item_id"], stopped["item_id"]);
    assert_eq!(committed["previous_item_id"], item);
    let (completed, _) = voice::until(
        &mut socket,
        "conversation.item.input_audio_transcription.completed",
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(completed["item_id"], stopped["item_id"]);

    voice::send(&mut socket, json!({"type": "session.update", "session": {"type": "transcription", "audio": {"input": {"transcription": {"model": "fixture.two"}, "turn_detection": null}}}})).await;
    let ga = voice::receive(&mut socket, Duration::from_secs(5)).await;
    assert_eq!(ga["type"], "session.updated");
    assert_eq!(
        ga["session"]["audio"]["input"]["transcription"]["model"],
        "fixture.two"
    );
    voice::append(&mut socket, &voice::tone(24_000, 300, 0.3)).await;
    voice::send(&mut socket, json!({"type": "input_audio_buffer.commit"})).await;
    let (failed, _) = voice::until(
        &mut socket,
        "conversation.item.input_audio_transcription.failed",
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(failed["error"]["code"], "unsupported_operation");
    voice::send(&mut socket, json!({"type": "response.create"})).await;
    assert_eq!(
        voice::receive(&mut socket, Duration::from_secs(5)).await["error"]["code"],
        "unsupported_event"
    );
    drop(socket);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
}
