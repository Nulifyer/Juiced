mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use support::{
    Server,
    speech::{self, Part},
};

async fn real_server(with_chat: bool) -> (Server, Vec<u8>) {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_WHISPER_SERVER").expect("set JUICED_TEST_WHISPER_SERVER"),
    );
    let model = PathBuf::from(
        std::env::var_os("JUICED_TEST_WHISPER_MODEL").expect("set JUICED_TEST_WHISPER_MODEL"),
    );
    let bundle_path = PathBuf::from(
        std::env::var_os("JUICED_TEST_WHISPER_BUNDLE")
            .expect("set JUICED_TEST_WHISPER_BUNDLE to a verified runtime-bundle JSON manifest"),
    );
    assert!(engine.is_file());
    assert!(model.is_file());
    let bundle: Value = serde_json::from_slice(&std::fs::read(bundle_path).unwrap()).unwrap();
    assert_eq!(bundle["implementation_id"], "whisper-cpp");
    let expected =
        Path::new(bundle["root"].as_str().unwrap()).join(bundle["entrypoint"].as_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(&engine).unwrap(),
        std::fs::canonicalize(expected).unwrap()
    );
    let wav = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .join("test/test_speech_de.wav");
    let audio = std::fs::read(wav).unwrap();
    assert!(audio.starts_with(b"RIFF"));
    let mut server = Server::prepare(true, |config| {
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(60000);
        let chat = config["models"][1].clone();
        config["models"] = json!([{"id":"tiny-real-speech","provider":"whisper-cpp","runtime_bundle":bundle,"model_file":model,"command":engine,"args":["--host","127.0.0.1","--port","{port}","-m","{model}","--threads","2","--no-gpu"],"resident_bytes":256u64*1024*1024,"max_inflight":1,"operations":["audio/transcriptions"],"policy":"on-demand"}]);
        if with_chat {
            config["models"].as_array_mut().unwrap().push(chat);
        }
    });
    server.isolate_loader_environment();
    server.start().await;
    #[cfg(target_os = "linux")]
    {
        let environment = std::fs::read(format!("/proc/{}/environ", server.pid())).unwrap();
        for name in [
            b"LD_LIBRARY_PATH=".as_slice(),
            b"LD_PRELOAD=".as_slice(),
            b"LD_AUDIT=".as_slice(),
        ] {
            assert!(
                !environment
                    .split(|byte| *byte == 0)
                    .any(|value| value.starts_with(name))
            );
        }
    }
    server.client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    (server, audio)
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_WHISPER_SERVER, JUICED_TEST_WHISPER_MODEL and JUICED_TEST_WHISPER_BUNDLE; CPU data pipeline only"]
async fn actual_cpu_whisper_bundle_transcribes_checked_in_wav_and_restarts_without_daemon_loader_environment()
 {
    let (mut server, audio) = real_server(false).await;
    let mut previous_pid = Value::Null;
    for (prefix, format) in [("/v1", "json"), ("/api/v1", "text")] {
        let (status, loaded) = server
            .post("load", json!({"model":"tiny-real-speech"}))
            .await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
        assert_eq!(loaded["loaded"], true);
        assert_eq!(loaded["implementation_id"], "whisper-cpp");
        assert_ne!(loaded["pid"], previous_pid);
        previous_pid = loaded["pid"].clone();
        assert_eq!(
            loaded["runtime_identity"]["bundle_digest"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        #[cfg(target_os = "linux")]
        assert_eq!(loaded["runtime_identity"]["state"], "observed");
        let response = speech::send(
            &server,
            &format!("{prefix}/audio/transcriptions"),
            &[
                Part::Text("model", "tiny-real-speech"),
                Part::Text("language", "de"),
                Part::Text("response_format", format),
                Part::File("file", "test_speech_de.wav", "audio/wav", &audio),
            ],
            server.read_key(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        if format == "json" {
            assert!(response.json::<Value>().await.unwrap()["text"].is_string());
        } else {
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/plain")
            );
            response.text().await.unwrap();
        }
        assert_eq!(server.model("tiny-real-speech").await["pid"], previous_pid);
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
        assert_eq!(
            server
                .post("unload", json!({"model":"tiny-real-speech"}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
        server.restart().await;
        assert_eq!(server.model("tiny-real-speech").await["loaded"], false);
    }
}

#[cfg(target_os = "linux")]
fn repeated_pcm_wave(source: &[u8], repeats: usize) -> Vec<u8> {
    assert!(source.starts_with(b"RIFF") && source.get(8..12) == Some(b"WAVE"));
    let mut offset = 12usize;
    loop {
        let header = source
            .get(offset..offset + 8)
            .expect("bounded WAV chunk header");
        let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let bytes = source
            .get(start..start + length)
            .expect("bounded WAV chunk");
        if &header[..4] == b"data" {
            let mut output = Vec::with_capacity(start + length * repeats);
            output.extend_from_slice(&source[..start]);
            for _ in 0..repeats {
                output.extend_from_slice(bytes);
            }
            let total = u32::try_from(output.len() - 8).unwrap();
            output[4..8].copy_from_slice(&total.to_le_bytes());
            output[offset + 4..start]
                .copy_from_slice(&u32::try_from(length * repeats).unwrap().to_le_bytes());
            assert!(output.len() < 8 * 1024 * 1024);
            return output;
        }
        offset = start + length + (length % 2);
    }
}
#[cfg(target_os = "linux")]
fn cpu_ticks(pid: u64) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let (_, tail) = stat.rsplit_once(") ").unwrap();
    let fields = tail.split_whitespace().collect::<Vec<_>>();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_WHISPER_SERVER, JUICED_TEST_WHISPER_MODEL and JUICED_TEST_WHISPER_BUNDLE; Linux CPU cancellation/owned-generation settlement only"]
async fn actual_cpu_whisper_disconnect_retires_its_generation_after_observed_cpu_work_and_then_reloads()
 {
    let (server, audio) = real_server(true).await;
    let (status, chat) = server.post("load", json!({"model":"fixture.two"})).await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let (status, before) = server
        .post("load", json!({"model":"tiny-real-speech"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    let warmup = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::Text("model", "tiny-real-speech"),
            Part::Text("language", "de"),
            Part::File("file", "test_speech_de.wav", "audio/wav", &audio),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(warmup.status(), StatusCode::OK);
    assert!(warmup.json::<Value>().await.unwrap()["text"].is_string());
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let pid = before["pid"].as_u64().unwrap();
    let baseline = cpu_ticks(pid);
    let rate = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    assert!(rate > 0);
    let required = (rate as u64).div_ceil(10);
    let long_audio = repeated_pcm_wave(&audio, 24);
    let held = speech::raw(
        &server,
        &[
            Part::Text("model", "tiny-real-speech"),
            Part::Text("language", "de"),
            Part::File(
                "file",
                "repeated_valid_speech.wav",
                "audio/wav",
                &long_audio,
            ),
        ],
    )
    .await;
    server
        .wait_for(async || {
            cpu_ticks(pid).saturating_sub(baseline) >= required
                && server.get("juiced/state").await["resources"]["active_requests"] == 1
        })
        .await;
    let mut byte = [0u8; 1];
    match held.try_read(&mut byte) {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        _ => panic!(
            "valid long transcription completed or disconnected before cancellation observation"
        ),
    }
    assert_eq!(server.model("tiny-real-speech").await["pid"], before["pid"]);
    drop(held);
    server
        .wait_for(async || {
            let model = server.model("tiny-real-speech").await;
            let resources = server.get("juiced/state").await["resources"].clone();
            model["loaded"] == false
                && model["pid"].is_null()
                && resources["active_requests"] == 0
                && resources["reserved_bytes"] == 64
                && resources["power"]["active"] == 0
        })
        .await;
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "owned whisper PID survived physical settlement acknowledgement"
    );
    assert_eq!(server.model("fixture.two").await["pid"], chat["pid"]);
    let (status,result)=server.post("completions",json!({"model":"fixture.two","prompt":"Unrelated chat after actual speech cancellation"})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["model"], "fixture.two");
    assert_eq!(server.model("fixture.two").await["pid"], chat["pid"]);
    let (status, after) = server
        .post("load", json!({"model":"tiny-real-speech"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_ne!(after["pid"], before["pid"]);
    assert!(
        after["process_lifecycle"]["generation"].as_u64().unwrap()
            > before["process_lifecycle"]["generation"].as_u64().unwrap()
    );
    assert_eq!(after["policy"], before["policy"]);
    assert_eq!(after["revision"], before["revision"]);
    let response = speech::send(
        &server,
        "/api/v1/audio/transcriptions",
        &[
            Part::Text("model", "tiny-real-speech"),
            Part::Text("language", "de"),
            Part::File("file", "test_speech_de.wav", "audio/wav", &audio),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    assert_eq!(server.model("tiny-real-speech").await["pid"], after["pid"]);
    let (status, unloaded) = server
        .post("unload", json!({"model":"tiny-real-speech"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{unloaded}");
    assert!(!Path::new(&format!("/proc/{}", after["pid"].as_u64().unwrap())).exists());
}
