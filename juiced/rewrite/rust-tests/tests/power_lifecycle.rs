mod support;

#[cfg(target_os = "linux")]
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::path::PathBuf;
use support::{
    Server,
    speech::{self, Part},
};

async fn gated_speech(enabled: bool) -> (Server, [PathBuf; 2]) {
    let mut release_files = None;
    let server = Server::configured(true, |config| {
        config["inhibit_suspend"] = json!(enabled);
        let root = std::path::Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap();
        let first = root.join("power-release-one");
        let second = root.join("power-release-two");
        let mut models = Vec::new();
        for (id, path) in [("fixture.one", &first), ("fixture.two", &second)] {
            let mut model = config["models"][0].clone();
            model["id"] = json!(id);
            model["provider"] = json!("whisper-cpp");
            model["max_inflight"] = json!(1);
            model["operations"] = json!(["audio/transcriptions"]);
            model["args"][5] = json!(root.join(format!("{id}.events")));
            model["args"].as_array_mut().unwrap().extend([
                json!("--host"),
                json!("127.0.0.1"),
                json!("--speech-physical-release-file"),
                json!(path),
            ]);
            models.push(model);
        }
        config["models"] = json!(models);
        release_files = Some([first, second]);
    })
    .await;
    (server, release_files.unwrap())
}
async fn power(server: &Server) -> Value {
    server.get("juiced/state").await["resources"]["power"].clone()
}
fn inactive_native(snapshot: &Value) {
    assert_eq!(snapshot["schema_version"], 1);
    assert_eq!(snapshot["enabled"], false);
    for field in ["attempts", "acquisitions", "releases"] {
        assert_eq!(snapshot[field], 0, "{field}");
    }
    for field in ["held", "acquiring", "releasing", "idle_grace"] {
        assert_eq!(snapshot[field], false, "{field}");
    }
    assert!(snapshot["last_error"].is_null());
}
#[cfg(target_os = "linux")]
fn power_threads(server: &Server) -> usize {
    std::fs::read_dir(format!("/proc/{}/task", server.pid()))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "juiced-power")
        })
        .count()
}

#[tokio::test]
async fn disabled_power_tracks_request_scopes_without_native_acquisition_or_power_thread() {
    let (server, _) = gated_speech(false).await;
    let idle = power(&server).await;
    inactive_native(&idle);
    assert_eq!(idle["active"], 0);
    assert_eq!(idle["settled"], true);
    #[cfg(target_os = "linux")]
    assert_eq!(power_threads(&server), 0);
    let audio = speech::audio();
    let parts = [
        Part::Text("model", "fixture.one"),
        Part::File("file", "fixture.wav", "audio/wav", &audio),
    ];
    let held = speech::raw(&server, &parts).await;
    server
        .wait_for(async || server.event("fixture.one", "speech-physical", "started"))
        .await;
    let active = power(&server).await;
    inactive_native(&active);
    assert_eq!(active["active"], 1);
    #[cfg(target_os = "linux")]
    assert_eq!(power_threads(&server), 0);
    let pid = server.model("fixture.one").await["pid"].clone();
    drop(held);
    server
        .wait_for(async || {
            server.model("fixture.one").await["pid"].is_null()
                && power(&server).await["active"] == 0
        })
        .await;
    let settled = power(&server).await;
    inactive_native(&settled);
    assert_eq!(settled["settled"], true);
    assert!(!server.event("fixture.one", "speech-physical", "settled"));
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{}", pid.as_u64().unwrap())).exists());
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
    #[cfg(target_os = "linux")]
    assert_eq!(power_threads(&server), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_POWER_INHIBIT=1 and accessible logind granting sleep:idle block inhibition"]
async fn real_logind_daemon_requests_share_a_lease_cancel_release_and_settle_on_shutdown() {
    assert_eq!(
        std::env::var("JUICED_TEST_POWER_INHIBIT").ok().as_deref(),
        Some("1"),
        "set JUICED_TEST_POWER_INHIBIT=1 only for the explicit host logind acceptance lane"
    );
    let (mut server, release) = gated_speech(true).await;
    let initial = power(&server).await;
    assert_eq!(initial["enabled"], true);
    assert_eq!(initial["platform"], "linux-logind");
    assert_eq!(initial["attempts"], 0);
    let audio = speech::audio();
    let first_parts = [
        Part::Text("model", "fixture.one"),
        Part::File("file", "fixture.wav", "audio/wav", &audio),
    ];
    let second_parts = [
        Part::Text("model", "fixture.two"),
        Part::File("file", "fixture.wav", "audio/wav", &audio),
    ];
    let first = speech::raw(&server, &first_parts).await;
    server
        .wait_for(async || server.event("fixture.one", "speech-physical", "started"))
        .await;
    let second = speech::raw(&server, &second_parts).await;
    server
        .wait_for(async || server.event("fixture.two", "speech-physical", "started"))
        .await;
    let shared = power(&server).await;
    assert_eq!(
        shared["held"], true,
        "positive logind acknowledgement required: {shared}"
    );
    assert_eq!(shared["active"], 2);
    assert_eq!(shared["attempts"], 1);
    assert_eq!(shared["acquisitions"], 1);
    assert_eq!(shared["releases"], 0);
    assert_eq!(power_threads(&server), 1);
    let first_pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
    let second_pid = server.model("fixture.two").await["pid"].as_u64().unwrap();
    drop(first);
    server
        .wait_for(async || {
            server.model("fixture.one").await["pid"].is_null()
                && power(&server).await["active"] == 1
        })
        .await;
    let overlap = power(&server).await;
    assert_eq!(overlap["held"], true);
    assert_eq!(overlap["acquisitions"], 1);
    assert_eq!(overlap["releases"], 0);
    assert!(!std::path::Path::new(&format!("/proc/{first_pid}")).exists());
    assert!(!server.event("fixture.one", "speech-physical", "settled"));
    assert_eq!(server.model("fixture.two").await["pid"], second_pid);
    std::fs::write(&release[0], b"release first speech fixture").unwrap();
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &first_parts,
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    assert_ne!(server.model("fixture.one").await["pid"], first_pid);
    server
        .wait_for(async || power(&server).await["active"] == 1)
        .await;
    assert_eq!(power(&server).await["acquisitions"], 1);
    let cancellation = std::time::Instant::now();
    drop(second);
    server
        .wait_for(async || {
            let snapshot = power(&server).await;
            server.model("fixture.two").await["pid"].is_null()
                && snapshot["active"] == 0
                && snapshot["held"] == false
                && snapshot["releases"] == 1
                && snapshot["settled"] == true
        })
        .await;
    assert!(cancellation.elapsed() >= std::time::Duration::from_millis(250));
    assert!(!std::path::Path::new(&format!("/proc/{second_pid}")).exists());
    assert!(!server.event("fixture.two", "speech-physical", "settled"));
    let idle = power(&server).await;
    assert_eq!(idle["acquisitions"], 1);
    assert!(idle["last_error"].is_null());
    for field in ["acquiring", "releasing", "idle_grace"] {
        assert_eq!(idle[field], false, "{field}");
    }
    std::fs::remove_file(&release[0]).unwrap();
    let started = server.event_count("fixture.one", "speech-physical", "started");
    let held = speech::raw(&server, &first_parts).await;
    server
        .wait_for(async || {
            server.event_count("fixture.one", "speech-physical", "started") > started
        })
        .await;
    let busy = power(&server).await;
    assert_eq!(busy["held"], true);
    assert_eq!(busy["acquisitions"], 2);
    let (status, second) = server.post("load", json!({"model":"fixture.two"})).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_ne!(second["pid"], second_pid);
    let pids = [
        server.model("fixture.one").await["pid"].as_u64().unwrap(),
        server.model("fixture.two").await["pid"].as_u64().unwrap(),
    ];
    assert_eq!(
        unsafe { libc::kill(server.pid() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = server
        .wait_for_manager_exit(std::time::Duration::from_secs(5))
        .await;
    server.retire_reaped_manager_without_group_signal();
    drop(held);
    assert!(
        status.success(),
        "manager did not confirm worker and inhibitor shutdown settlement"
    );
    for pid in pids {
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "owned worker survived settled shutdown"
        );
    }
}
