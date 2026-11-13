mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use support::{Server, huggingface::REVISION};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const REPO: &str = "fixture/Slow-GGUF";
const FILE: &str = "Slow-Q4_K_M.gguf";
const LENGTH: usize = 3 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const ETAG: &str = "\"slow-v1\"";

/// A hub whose file endpoint honours `Range` and streams slowly while `throttle` is set, so a
/// test can stop the manager mid-transfer.
struct SlowHub {
    base: String,
    bytes: Arc<Vec<u8>>,
    ranges: Arc<Mutex<Vec<Option<String>>>>,
    throttle: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for SlowHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SlowHub {
    async fn start() -> Self {
        let bytes: Arc<Vec<u8>> = Arc::new((0..LENGTH).map(|index| (index % 251) as u8).collect());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let throttle = Arc::new(AtomicBool::new(true));
        let (served, recorded, slow) = (bytes.clone(), ranges.clone(), throttle.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(
                    socket,
                    served.clone(),
                    recorded.clone(),
                    slow.clone(),
                ));
            }
        });
        Self {
            base,
            bytes,
            ranges,
            throttle,
            task,
        }
    }

    fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(self.bytes.as_slice()))
    }

    fn resumed_offsets(&self) -> Vec<u64> {
        self.ranges
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .filter_map(|range| {
                range
                    .strip_prefix("bytes=")?
                    .strip_suffix('-')?
                    .parse()
                    .ok()
            })
            .collect()
    }
}

async fn serve(
    mut socket: TcpStream,
    bytes: Arc<Vec<u8>>,
    ranges: Arc<Mutex<Vec<Option<String>>>>,
    throttle: Arc<AtomicBool>,
) {
    let mut request = Vec::new();
    while request.len() < 16384 && !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        if socket.read_exact(&mut byte).await.is_err() {
            return;
        }
        request.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let target = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.split(' ').next())
        .unwrap_or("")
        .to_owned();
    let header = |name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let head = |status: &str, headers: &str, length: usize| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {length}\r\n{headers}Connection: close\r\n\r\n"
        )
    };
    if path.starts_with(&format!("/api/models/{REPO}")) && query.contains("blobs=true") {
        let sha = format!("{:x}", Sha256::digest(bytes.as_slice()));
        let body = serde_json::to_vec(&json!({
            "id": REPO, "sha": REVISION, "pipeline_tag": "text-generation",
            "siblings": [{"rfilename": FILE, "size": LENGTH, "lfs": {"sha256": sha, "size": LENGTH}}],
        }))
        .unwrap();
        let reply = head("200 OK", "Content-Type: application/json\r\n", body.len());
        let _ = socket.write_all(reply.as_bytes()).await;
        let _ = socket.write_all(&body).await;
        return;
    }
    if path != format!("/{REPO}/resolve/{REVISION}/{FILE}") {
        let _ = socket
            .write_all(head("404 Not Found", "", 0).as_bytes())
            .await;
        return;
    }
    let range = header("range");
    ranges.lock().unwrap().push(range.clone());
    let offset = range
        .as_deref()
        .filter(|_| header("if-range").as_deref() == Some(ETAG))
        .and_then(|range| {
            range
                .strip_prefix("bytes=")?
                .strip_suffix('-')?
                .parse()
                .ok()
        })
        .filter(|offset: &usize| *offset < bytes.len())
        .unwrap_or(0);
    let reply = if offset > 0 {
        head(
            "206 Partial Content",
            &format!(
                "ETag: {ETAG}\r\nContent-Range: bytes {offset}-{}/{}\r\n",
                bytes.len() - 1,
                bytes.len()
            ),
            bytes.len() - offset,
        )
    } else {
        head("200 OK", &format!("ETag: {ETAG}\r\n"), bytes.len())
    };
    if socket.write_all(reply.as_bytes()).await.is_err() {
        return;
    }
    for chunk in bytes[offset..].chunks(CHUNK) {
        if socket.write_all(chunk).await.is_err() {
            return;
        }
        if throttle.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }
}

async fn server(hub: &SlowHub, edit: impl FnOnce(&mut Value)) -> Server {
    let mut server = Server::prepare(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
        edit(config);
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.start().await;
    server
}

async fn until(what: &str, mut condition: impl AsyncFnMut() -> Option<Value>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(value) = condition().await {
            return value;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn pull(server: &Server) -> Value {
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let (status, view) = server
        .post(
            "juiced/library/pull",
            json!({"repo": REPO, "file": FILE, "expected_generation": generation}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn pull_view(server: &Server, id: &str) -> Value {
    server.get(&format!("juiced/library/pulls/{id}")).await
}

async fn job_of(server: &Server, pull: &str) -> Value {
    let pulled = pull_view(server, pull).await;
    let job = pulled["files"][0]["job"].as_str().expect("pull owns a job");
    server.get(&format!("juiced/artifacts/jobs/{job}")).await
}

async fn partway(server: &Server, pull: &str) -> Value {
    until("partial progress", async || {
        let job = job_of(server, pull).await;
        let done = job["downloaded_bytes"].as_u64().unwrap_or(0);
        (done > (LENGTH / 8) as u64 && done < LENGTH as u64 / 2).then_some(job)
    })
    .await
}

#[tokio::test]
async fn crashed_library_pull_survives_restart_and_resumes_from_committed_bytes() {
    let hub = SlowHub::start().await;
    let mut server = server(&hub, |_| {}).await;
    let started = pull(&server).await;
    let id = started["pull"].as_str().unwrap().to_owned();
    partway(&server, &id).await;
    server.crash();
    let journal: Value = serde_json::from_slice(
        &std::fs::read(server.directory.path().join("acquisitions.json")).unwrap(),
    )
    .unwrap();
    assert!(journal["pulls"][&id].is_object(), "{journal}");
    hub.throttle.store(false, Ordering::Release);
    server.start().await;
    let listed = server.get("juiced/library/pulls").await;
    assert!(
        listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pull| pull["pull"] == id),
        "{listed}"
    );
    let done = until("pull completion", async || {
        let view = pull_view(&server, &id).await;
        matches!(view["state"].as_str(), Some("complete" | "failed")).then_some(view)
    })
    .await;
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(done["files"][0]["sha256"], hub.sha256());
    assert!(
        hub.resumed_offsets().iter().any(|offset| *offset > 0),
        "restart should continue from committed bytes: {:?}",
        hub.ranges.lock().unwrap()
    );
    let stored = server.get("juiced/artifacts").await;
    assert!(
        stored["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["artifact"]["sha256"] == hub.sha256() || row["sha256"] == hub.sha256()),
        "{stored}"
    );
}

#[tokio::test]
async fn paused_pull_stays_paused_across_restart_until_resumed() {
    let hub = SlowHub::start().await;
    let mut server = server(&hub, |_| {}).await;
    let id = pull(&server).await["pull"].as_str().unwrap().to_owned();
    let job = partway(&server, &id).await;
    let job_id = job["id"].as_str().unwrap().to_owned();
    let (status, paused) = server
        .post(
            &format!("juiced/artifacts/jobs/{job_id}"),
            json!({"action": "pause", "expected_revision": job["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    until("pause", async || {
        let job = server.get(&format!("juiced/artifacts/jobs/{job_id}")).await;
        (job["state"] == "paused" && job["writer_active"] == false).then_some(job)
    })
    .await;
    server.stop();
    server.start().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let job = server.get(&format!("juiced/artifacts/jobs/{job_id}")).await;
    assert_eq!(job["state"], "paused", "{job}");
    assert_eq!(pull_view(&server, &id).await["state"], "waiting");
    hub.throttle.store(false, Ordering::Release);
    let (status, resumed) = server
        .post(
            &format!("juiced/artifacts/jobs/{job_id}"),
            json!({"action": "resume", "expected_revision": job["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    let done = until("pull completion", async || {
        let view = pull_view(&server, &id).await;
        (view["state"] == "complete").then_some(view)
    })
    .await;
    assert_eq!(done["files"][0]["state"], "present");
}

#[tokio::test]
async fn native_pull_job_is_listed_after_restart_and_continues() {
    let hub = SlowHub::start().await;
    let mut server = server(&hub, |_| {}).await;
    let (status, started) = server
        .json(
            Method::POST,
            "/api/v1/pull",
            Some(json!({"model_name": "user.Slow", "checkpoint": format!("{REPO}:Q4_K_M"), "recipe": "llamacpp", "stream": true, "subscribe": false})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let downloads = async |server: &Server| {
        let (status, rows) = server
            .json(Method::GET, "/api/v1/downloads", None, server.admin_key())
            .await;
        assert_eq!(status, StatusCode::OK);
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "model:user.Slow")
            .cloned()
    };
    until("native progress", async || {
        downloads(&server)
            .await
            .filter(|row| row["bytes_downloaded"].as_u64().unwrap_or(0) > (LENGTH / 8) as u64)
    })
    .await;
    server.crash();
    hub.throttle.store(false, Ordering::Release);
    server.start().await;
    let listed = until("native job after restart", async || {
        downloads(&server).await
    })
    .await;
    assert_eq!(listed["model_name"], "user.Slow");
    let settled = until("native job settles", async || {
        downloads(&server)
            .await
            .filter(|row| row["running"] == false)
    })
    .await;
    // The synthetic bytes are not a real GGUF, so registration refuses once the weights land.
    assert_eq!(settled["bytes_downloaded"], LENGTH, "{settled}");
    assert!(
        matches!(settled["status"].as_str(), Some("completed" | "error")),
        "{settled}"
    );
    assert!(hub.resumed_offsets().iter().any(|offset| *offset > 0));
}

#[tokio::test]
async fn storage_view_reports_stores_and_reclaim_previews_before_reference_checked_deletion() {
    let hub = SlowHub::start().await;
    hub.throttle.store(false, Ordering::Release);
    let mut server = Server::prepare(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
    });
    let root = server.directory.path().to_owned();
    std::fs::create_dir_all(root.join("cache/llama/bin")).unwrap();
    std::fs::write(root.join("cache/llama/bin/llama-server"), [7u8; 1000]).unwrap();
    std::fs::create_dir_all(root.join("prompt-state")).unwrap();
    std::fs::write(root.join("prompt-state/slot.bin"), [1u8; 300]).unwrap();
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.start().await;
    let id = pull(&server).await["pull"].as_str().unwrap().to_owned();
    until("pull completion", async || {
        let view = pull_view(&server, &id).await;
        (view["state"] == "complete").then_some(view)
    })
    .await;

    for prefix in ["/api/v0", "/api/v1", "/v0", "/v1"] {
        let (status, summary) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/storage?summary=1"),
                None,
                Some(support::CLIENT_KEY),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{prefix}: {summary}");
        assert_eq!(summary["summary"], true);
        assert!(!summary["volumes"].as_array().unwrap().is_empty());
    }
    let view = server.get("juiced/storage").await;
    assert_eq!(view["schema_version"], 1);
    let categories: Vec<&str> = view["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|category| category["id"].as_str().unwrap())
        .collect();
    for expected in [
        "models",
        "library",
        "downloads",
        "engines",
        "session_cache",
        "config",
        "logs",
        "legacy",
        "other",
    ] {
        assert!(
            categories.contains(&expected),
            "{expected} in {categories:?}"
        );
    }
    let legacy = |id: &str| {
        view["legacy"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} in {}", view["legacy"]))
    };
    assert_eq!(legacy("legacy:cache")["bytes"], 1000);
    assert_eq!(legacy("legacy:cache")["reclaimable"], true);
    assert_eq!(legacy("legacy:prompt-state")["bytes"], 300);
    let pulled = view["unreferenced"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["bytes"] == LENGTH)
        .cloned()
        .unwrap_or_else(|| panic!("pulled file unreferenced: {}", view["unreferenced"]));
    let fixture = view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model"] == "fixture.one")
        .cloned()
        .unwrap();
    assert_eq!(fixture["removal_frees_bytes"], 0, "{fixture}");

    let items = json!([pulled["id"], "legacy:cache", "engine:missing"]);
    let (status, preview) = server
        .post(
            "juiced/storage/reclaim",
            json!({"items": items, "preview": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["freed_bytes"], LENGTH as u64 + 1000);
    assert_eq!(preview["refused"], 1);
    let (status, refused) = server
        .post(
            "juiced/storage/reclaim",
            json!({"items": items, "expected_revision": preview["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "storage_item_refused");
    let items = json!([pulled["id"], "legacy:cache"]);
    let (status, missing) = server
        .post("juiced/storage/reclaim", json!({"items": items}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    let (status, stale) = server
        .post(
            "juiced/storage/reclaim",
            json!({"items": items, "expected_revision": "0000000000000000"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "storage_revision_conflict");
    let (_, preview) = server
        .post(
            "juiced/storage/reclaim",
            json!({"items": items, "preview": true}),
        )
        .await;
    let (status, applied) = server
        .post(
            "juiced/storage/reclaim",
            json!({"items": items, "expected_revision": preview["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(applied["freed_bytes"], LENGTH as u64 + 1000);
    assert!(!root.join("cache").exists());
    assert!(root.join("prompt-state/slot.bin").exists());
    assert!(root.join("fixture weights.gguf").exists());
    let after = server.get("juiced/storage").await;
    assert!(
        after["unreferenced"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != pulled["id"])
    );
    let (status, _) = server
        .json(
            Method::POST,
            "/v1/juiced/storage/reclaim",
            Some(json!({"items": ["legacy:prompt-state"], "preview": true})),
            Some(support::CLIENT_KEY),
        )
        .await;
    assert!(
        matches!(status, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
        "{status}"
    );
}

#[tokio::test]
async fn downloads_refuse_up_front_when_the_volume_would_drop_below_the_reserve() {
    let hub = SlowHub::start().await;
    let server = server(&hub, |config| {
        config["storage"] = json!({"reserve_bytes": 1u64 << 50, "low_space_bytes": 1u64 << 50});
    })
    .await;
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let (status, refused) = server
        .post(
            "juiced/library/pull",
            json!({"repo": REPO, "file": FILE, "expected_generation": generation}),
        )
        .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{refused}");
    assert_eq!(refused["error"]["code"], "insufficient_storage");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("short"), "{message}");
    assert!(
        server.get("juiced/library/pulls").await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let summary = server.get("juiced/storage?summary=1").await;
    assert!(
        summary["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "low_space"),
        "{summary}"
    );
}
