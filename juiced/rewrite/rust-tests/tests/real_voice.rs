mod support;

use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use support::{
    Server,
    voice::{self, Wyoming},
};

fn speech_wav() -> (u32, Vec<i16>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .join("test/test_speech.wav");
    let decoded = juiced_service::audio::decode_wav(&std::fs::read(path).unwrap()).unwrap();
    (decoded.rate, decoded.samples)
}

async fn real_server() -> Server {
    let variable =
        |name: &str| PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("set {name}")));
    let engine = variable("JUICED_TEST_WHISPER_SERVER");
    let model = variable("JUICED_TEST_WHISPER_MODEL");
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(variable("JUICED_TEST_WHISPER_BUNDLE")).unwrap())
            .unwrap();
    let mut server = Server::prepare(true, |config| {
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(60000);
        config["models"] = json!([{"id":"tiny-real-speech","provider":"whisper-cpp","runtime_bundle":bundle,"model_file":model,"command":engine,"args":["--host","127.0.0.1","--port","{port}","-m","{model}","--threads","2","--no-gpu"],"resident_bytes":256u64*1024*1024,"max_inflight":1,"operations":["audio/transcriptions"],"policy":"on-demand"}]);
    });
    server.isolate_loader_environment();
    server.start().await;
    server.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    server
}

fn words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect()
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_WHISPER_SERVER, JUICED_TEST_WHISPER_MODEL and JUICED_TEST_WHISPER_BUNDLE"]
async fn actual_whisper_transcribes_wyoming_and_realtime_audio_after_conversion() {
    let server = real_server().await;
    let (rate, samples) = speech_wav();
    assert_eq!(rate, 16_000);

    let view = voice::wyoming(&server, json!({"enabled": true, "listen": "127.0.0.1:0"})).await;
    let mut client = Wyoming::connect(view["listener"]["address"].as_str().unwrap()).await;
    client.send("describe", json!({}), &[]).await;
    let info = client.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(info.data["asr"][0]["models"][0]["name"], "tiny-real-speech");
    let resampled = juiced_service::audio::resample(&samples, 16_000, 44_100);
    let stereo: Vec<u8> = resampled
        .iter()
        .flat_map(|sample| [*sample, *sample])
        .flat_map(i16::to_le_bytes)
        .collect();
    let started = std::time::Instant::now();
    let transcript = client.transcribe("en", 44_100, 2, 2, &stereo).await;
    assert_eq!(transcript.kind, "transcript", "{:?}", transcript.data);
    let wyoming_text = transcript.data["text"].as_str().unwrap().to_owned();
    eprintln!(
        "Wyoming 44.1 kHz stereo -> whisper tiny ({} ms): {wyoming_text:?}",
        started.elapsed().as_millis()
    );
    assert!(words(&wyoming_text).contains("working"), "{wyoming_text}");

    let (mut socket, _) = voice::realtime(
        &server,
        "/v1/realtime?intent=transcription&model=tiny-real-speech",
        server.read_key(),
        None,
    )
    .await
    .unwrap();
    voice::receive(&mut socket, Duration::from_secs(5)).await;
    voice::send(
        &mut socket,
        json!({"type": "transcription_session.update", "session": {"input_audio_transcription": {"language": "en"}, "turn_detection": {"type": "server_vad", "silence_duration_ms": 600}}}),
    )
    .await;
    voice::until(
        &mut socket,
        "transcription_session.updated",
        Duration::from_secs(5),
    )
    .await;
    let mut stream = vec![0i16; 24_000 / 2];
    stream.extend(juiced_service::audio::resample(&samples, 16_000, 24_000));
    stream.extend(vec![0i16; 24_000]);
    let started = std::time::Instant::now();
    voice::append(&mut socket, &stream).await;
    let (completed, seen) = voice::until(
        &mut socket,
        "conversation.item.input_audio_transcription.completed",
        Duration::from_secs(60),
    )
    .await;
    let realtime_text = completed["transcript"].as_str().unwrap().to_owned();
    eprintln!(
        "Realtime 24 kHz server_vad -> whisper tiny ({} ms, events {seen:?}): {realtime_text:?}",
        started.elapsed().as_millis()
    );
    assert!(
        seen.iter()
            .any(|kind| kind == "input_audio_buffer.speech_started")
    );
    assert!(
        seen.iter()
            .any(|kind| kind == "input_audio_buffer.committed")
    );
    assert!(words(&realtime_text).contains("working"), "{realtime_text}");
    let summary = server.get("juiced/wyoming").await["last_event"].clone();
    assert_eq!(summary["outcome"], "completed");
    assert_eq!(summary["type"], "transcribe");
}
