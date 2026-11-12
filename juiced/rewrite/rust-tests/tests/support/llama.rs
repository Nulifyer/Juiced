use super::Server;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Barrier;

pub async fn five_slot_pipeline(
    server: &mut Server,
    model_id: &str,
    loaded: Value,
    before: Value,
    protected: bool,
    engine: &Path,
    model: &Path,
) -> Value {
    const HOST_CAPACITY: u64 = 768 * 1024 * 1024;
    const HOST_RESIDENT: u64 = 128 * 1024 * 1024;
    const REQUEST_BYTES: u64 = 8 * 1024 * 1024;
    let engine_sha = super::filesystem::digest(engine);
    let model_sha = super::filesystem::digest(model);
    let config_before = std::fs::read(&server.config_path).unwrap();
    let pid = loaded["pid"].as_u64().unwrap();
    if protected {
        server.wait_for(async || server.get("juiced/admission").await["resources"]["cache_control_memory"]["active_operations"] == 0).await;
    }
    let idle_admission = server.get("juiced/admission").await;
    let metadata_bytes = if protected {
        let memory = &idle_admission["scheduler"]["memory"];
        let control = &idle_admission["resources"]["cache_control_memory"];
        assert_eq!(control["state"], "accounted");
        assert_eq!(memory["resident_bytes"], HOST_RESIDENT);
        assert_eq!(memory["capacity_bytes"], HOST_CAPACITY);
        let metadata = control["metadata_bytes"].as_u64().unwrap();
        assert_eq!(memory["reserved_bytes"], HOST_RESIDENT + metadata);
        metadata
    } else {
        0
    };
    assert_eq!(before["configured"]["context_tokens"], 2560);
    assert_eq!(before["applied"]["launch"]["parallel_slots"], 5);
    let slots = before["observed"]["slots"]
        .as_array()
        .expect("live engine slot observation");
    assert_eq!(slots.len(), 5);
    let contexts = slots
        .iter()
        .map(|slot| slot["context_tokens"].clone())
        .collect::<Vec<_>>();
    let mut command_line = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .unwrap()
        .take(65537)
        .read_to_end(&mut command_line)
        .unwrap();
    assert!(command_line.len() <= 65536);
    let arguments = command_line
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| std::str::from_utf8(part).unwrap())
        .collect::<Vec<_>>();
    for (flag, value) in [("--ctx-size", "2560"), ("--parallel", "5")] {
        assert!(
            arguments.windows(2).any(|pair| pair == [flag, value]),
            "missing applied {flag}"
        );
    }
    assert!(
        contexts
            .iter()
            .all(|tokens| tokens.as_u64().is_some_and(|tokens| tokens > 64))
    );
    let port = arguments
        .windows(2)
        .find(|pair| pair[0] == "--port")
        .unwrap()[1]
        .parse::<u16>()
        .unwrap();
    let worker_slots = format!("http://127.0.0.1:{port}/slots");
    let barrier = Arc::new(Barrier::new(9));
    let mut requests = Vec::new();
    for index in 0..8 {
        let model_id = model_id.to_owned();
        let client = server.client.clone();
        let url = format!("{}/v1/completions", server.base);
        let key = server.admin_key().unwrap();
        let barrier = barrier.clone();
        requests.push(tokio::spawn(async move {
            barrier.wait().await;
            let response=client.post(url).bearer_auth(key).header("x-juiced-session",format!("private-five-slot-session-{index}")).json(&json!({"model":model_id,"prompt":format!("private-five-slot-prompt-{index}: Once upon a time"),"max_tokens":64,"temperature":0,"ignore_eos":true,"cache_prompt":false,"stream":false})).send().await.unwrap();
            let status=response.status();let body=response.json::<Value>().await.unwrap();(status,body)
        }));
    }
    barrier.wait().await;
    let mut maximum_active = 0;
    let mut maximum_pending = 0;
    let mut maximum_processing = 0;
    let mut busy_context = Value::Null;
    let mut busy_admission = Value::Null;
    let observed = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let admission = server.get("juiced/admission").await;
            let pool = admission["scheduler"]["pools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|pool| pool["service"] == model_id)
                .unwrap();
            assert_eq!(pool["physical_capacity"], 5);
            let active = pool["active"].as_u64().unwrap();
            assert!(active <= 5);
            maximum_active = maximum_active.max(active);
            maximum_pending = maximum_pending.max(pool["pending"].as_u64().unwrap());
            let context = if protected {
                let mut response=server.client.get(&worker_slots).timeout(Duration::from_millis(500)).send().await.unwrap();assert!(response.status().is_success());let mut bytes=Vec::with_capacity(2048);
                while let Some(chunk)=response.chunk().await.unwrap() {assert!(chunk.len()<=1048576usize.saturating_sub(bytes.len()));bytes.extend_from_slice(&chunk);}
                let slots=juiced_service::profiles::parse_slots(&bytes).unwrap();json!({"observed":{"slots":slots},"scope":"read-only owned worker slots; no mutation or manager slot bypass for inference"})
            } else {server.get(&format!("juiced/context?model={model_id}")).await};
            if protected && active==5 {
                let memory=&admission["scheduler"]["memory"];assert_eq!(memory["resident_bytes"],HOST_RESIDENT);
                assert!(memory["reserved_bytes"].as_u64().unwrap()>=HOST_RESIDENT+metadata_bytes+5*REQUEST_BYTES);
                assert!(memory["reserved_bytes"].as_u64().unwrap()<=HOST_CAPACITY);assert_eq!(admission["resources"]["reserved_bytes"],memory["reserved_bytes"]);
                assert_eq!(admission["resources"]["reservation_source"],"declared-protected-host");busy_admission=admission;
            }
            if let Some(slots) = context["observed"]["slots"].as_array() {
                let processing = slots
                    .iter()
                    .filter(|slot| slot["is_processing"] == true)
                    .count();
                maximum_processing = maximum_processing.max(processing);
                assert_eq!(
                    slots
                        .iter()
                        .map(|slot| slot["context_tokens"].clone())
                        .collect::<Vec<_>>(),
                    contexts
                );
                if processing == 5 {
                    busy_context = context;
                }
            }
            if maximum_active == 5 && maximum_processing == 5 && maximum_pending > 0 {
                return true;
            }
            if requests.iter().all(|request| request.is_finished()) {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or(false);
    let mut completion_tokens = Vec::new();
    for request in requests {
        let (status, body) = request.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["model"], model_id);
        assert_eq!(body["usage"]["completion_tokens"], 64);
        completion_tokens.push(body["usage"]["completion_tokens"].as_u64().unwrap());
    }
    assert!(
        observed,
        "active={maximum_active},pending={maximum_pending},engine_processing={maximum_processing}"
    );
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let after = server
        .get(&format!("juiced/context?model={model_id}"))
        .await;
    assert_eq!(after["configured"], before["configured"]);
    let mut expected_applied = before["applied"].clone();
    assert!(
        after["applied"]["runtime_identity"]["verified_at_unix_ms"]
            .as_u64()
            .is_some_and(|time| time > 0)
    );
    expected_applied["runtime_identity"]["verified_at_unix_ms"] =
        after["applied"]["runtime_identity"]["verified_at_unix_ms"].clone();
    assert_eq!(after["applied"], expected_applied);
    assert_eq!(
        after["observed"]["slots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|slot| slot["context_tokens"].clone())
            .collect::<Vec<_>>(),
        contexts
    );
    assert!(
        after["observed"]["slots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|slot| slot["is_processing"] == false)
    );
    assert_eq!(server.model(model_id).await["pid"], pid);
    let stream=server.client.post(format!("{}/v1/completions",server.base)).bearer_auth(server.admin_key().unwrap()).json(&json!({"model":model_id,"prompt":"Short stream","max_tokens":2,"temperature":0,"stream":true})).send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let wire = stream.text().await.unwrap();
    let mut model_frames = 0;
    let mut done = false;
    for line in wire.lines().filter_map(|line| line.strip_prefix("data:")) {
        let payload = line.trim();
        if payload == "[DONE]" {
            done = true;
            continue;
        }
        let frame: Value = serde_json::from_str(payload).unwrap();
        assert!(frame.get("error").is_none());
        if let Some(model) = frame.get("model") {
            assert_eq!(model, model_id);
            model_frames += 1;
        }
    }
    assert!(done && model_frames > 0);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let trace = server.get("juiced/requests").await;
    let trace_text = serde_json::to_string(&trace).unwrap();
    assert!(!trace_text.contains("private-five-slot-"));
    let (status, unloaded) = server.post("unload", json!({"model":model_id})).await;
    assert_eq!(status, StatusCode::OK, "{unloaded}");
    assert_eq!(unloaded["loaded"], false);
    if protected {
        server
            .wait_for(async || {
                server.get("juiced/admission").await["scheduler"]["memory"]["reserved_bytes"]
                    == metadata_bytes
            })
            .await;
        let final_claims = server.get("juiced/admission").await;
        assert_eq!(final_claims["scheduler"]["memory"]["resident_bytes"], 0);
        assert_eq!(
            final_claims["resources"]["cache_control_memory"]["active_operations"],
            0
        );
    } else {
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
    }
    assert_eq!(std::fs::read(&server.config_path).unwrap(), config_before);
    assert_eq!(super::filesystem::digest(engine), engine_sha);
    assert_eq!(super::filesystem::digest(model), model_sha);
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    let manager_pid = server.pid();
    server.stop();
    assert!(!Path::new(&format!("/proc/{manager_pid}")).exists());
    json!({"schema_version":1,"scope":"Selected llama.cpp CPU five-slot pipeline with eight JSON API requests, SSE and queue pressure; declared host ceilings when protected, not resource-peak calibration, idle-context multiplexing or output quality","engine_sha256":engine_sha,"model_sha256":model_sha,"worker_pid":pid,"protected_host":protected,"idle_admission":idle_admission,"busy_admission":busy_admission,"maximum_manager_active":maximum_active,"maximum_pending":maximum_pending,"maximum_engine_processing":maximum_processing,"before":before,"busy":busy_context,"after":after,"completion_tokens":completion_tokens,"stream_model_frames":model_frames,"stream_done":done,"configuration_unchanged":true,"execution_inputs_unchanged":true,"unload_and_manager_exit_acknowledged":true})
}

#[cfg(target_os = "linux")]
pub fn flat_cpu_bundle(engine: &Path, root: &Path) -> (PathBuf, Value) {
    use super::filesystem::digest;
    use std::os::unix::fs::PermissionsExt;
    let source = std::fs::canonicalize(engine).expect("selected llama-server exists");
    assert!(std::fs::metadata(&source).unwrap().is_file());
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut sources = BTreeMap::from([("llama-server".to_owned(), source.clone())]);
    let mut enumerated = 0;
    for entry in std::fs::read_dir(source.parent().unwrap()).unwrap() {
        enumerated += 1;
        assert!(
            enumerated <= 1024,
            "selected engine directory exceeds bounded package scan"
        );
        let entry = entry.unwrap();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let family = name.starts_with("libllama.so")
            || name.starts_with("libllama-common.so")
            || name.starts_with("libllama-server-impl.so")
            || name.starts_with("libmtmd.so")
            || name.starts_with("libggml.so")
            || name.starts_with("libggml-base.so")
            || name.starts_with("libggml-cpu");
        if family && name.contains(".so") {
            let path = std::fs::canonicalize(entry.path()).unwrap();
            assert!(std::fs::metadata(&path).unwrap().is_file());
            sources.insert(name.to_owned(), path);
        }
    }
    assert!(
        sources.len() <= 64,
        "selected CPU package exceeds its file bound"
    );
    let mut bytes = 0;
    let files: Vec<_> = sources.into_iter().map(|(name, source)| {
        let metadata = std::fs::metadata(&source).unwrap();
        assert!(metadata.len() <= 256 * 1024 * 1024);
        bytes += metadata.len();
        assert!(bytes <= 1024 * 1024 * 1024);
        let before = digest(&source);
        let target = root.join(&name);
        std::fs::copy(&source, &target).unwrap();
        let executable = name == "llama-server";
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(if executable {0o700} else {0o600})).unwrap();
        assert!(!std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        assert_eq!(digest(&target), before, "selected package copy changed bytes");
        assert_eq!(digest(&source), before, "selected package source changed during copy");
        json!({"path":name,"sha256":before,"role":if executable {"executable"} else {"library"}})
    }).collect();
    (
        root.join("llama-server"),
        json!({"schema_version":1,"implementation_id":"llama-cpp","root":root,"entrypoint":"llama-server","library_directories":["."],"files":files}),
    )
}
