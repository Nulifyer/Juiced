#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Original checkpoint folders (Transformers safetensors, ONNX classifiers) acquired through
//! the library: search, a durable pull that learns the digests of files the hub keeps outside
//! LFS, resume after a crash, registration on the engine that reads the folder, and storage.

mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use support::{CLIENT_KEY, Server, binary};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const CHAT: &str = "fixture/Tiny-Transformers";
const CLASSIFIER: &str = "fixture/Tiny-Classifier-ONNX";
const WEIGHTS: usize = 3 * 1024 * 1024;
const TOKEN: &str = "hf_fixture_token";

struct File {
    path: &'static str,
    bytes: Vec<u8>,
    lfs: bool,
}

impl File {
    fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.bytes))
    }
    fn blob(&self) -> String {
        let mut hash = Sha1::new();
        hash.update(format!("blob {}\0", self.bytes.len()).as_bytes());
        hash.update(&self.bytes);
        format!("{:x}", hash.finalize())
    }
}

struct Repo {
    pipeline: &'static str,
    tags: &'static [&'static str],
    gated: bool,
    files: Vec<File>,
}

/// A hub that keeps small files outside LFS (no SHA-256, a git blob id, a same-host redirect
/// like Hugging Face's resolve cache), honours `Range` and streams the large weights slowly
/// while `throttle` is set.
struct Hub {
    base: String,
    repos: Arc<BTreeMap<&'static str, Repo>>,
    throttle: Arc<AtomicBool>,
    ranges: Arc<Mutex<Vec<String>>>,
    authorized: Arc<Mutex<Vec<(String, bool)>>>,
    task: JoinHandle<()>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn pattern(seed: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| (index % 251) as u8 ^ seed)
        .collect()
}

fn repos() -> BTreeMap<&'static str, Repo> {
    let small = |path, text: &str| File {
        path,
        bytes: text.as_bytes().to_vec(),
        lfs: false,
    };
    BTreeMap::from([
        (
            CHAT,
            Repo {
                pipeline: "text-generation",
                tags: &[
                    "transformers",
                    "safetensors",
                    "text-generation",
                    "license:apache-2.0",
                ],
                gated: false,
                files: vec![
                    small(
                        "config.json",
                        r#"{"architectures":["Qwen3ForCausalLM"],"hidden_size":64,"num_attention_heads":4,"num_key_value_heads":2,"num_hidden_layers":2,"head_dim":16,"max_position_embeddings":4096,"torch_dtype":"bfloat16"}"#,
                    ),
                    small(
                        "generation_config.json",
                        r#"{"do_sample":true,"temperature":0.6}"#,
                    ),
                    small("tokenizer_config.json", r#"{"model_max_length":4096}"#),
                    File {
                        path: "tokenizer.json",
                        bytes: pattern(7, 40_000),
                        lfs: true,
                    },
                    File {
                        path: "model.safetensors",
                        bytes: pattern(3, WEIGHTS),
                        lfs: true,
                    },
                    small("README.md", "# Tiny"),
                    small(".gitattributes", "*.safetensors filter=lfs"),
                ],
            },
        ),
        (
            CLASSIFIER,
            Repo {
                pipeline: "text-classification",
                tags: &[
                    "onnx",
                    "distilbert",
                    "text-classification",
                    "license:apache-2.0",
                ],
                gated: true,
                files: vec![
                    small(
                        "config.json",
                        r#"{"architectures":["DistilBertForSequenceClassification"],"id2label":{"0":"safe","1":"phishing"}}"#,
                    ),
                    small(
                        "tokenizer.json",
                        r#"{"version":"1.0","model":{"type":"WordPiece"}}"#,
                    ),
                    small("vocab.txt", "[PAD]\n[UNK]\nhello\n"),
                    File {
                        path: "model.onnx",
                        bytes: [b"\x08\x07".as_slice(), &pattern(9, 60_000)].concat(),
                        lfs: true,
                    },
                    small("export.py", "print('not needed')"),
                ],
            },
        ),
    ])
}

fn head(status: &str, headers: &str, length: usize) -> String {
    format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\n{headers}Connection: close\r\n\r\n")
}

fn info(repo: &str, entry: &Repo) -> Value {
    let siblings: Vec<Value> = entry
        .files
        .iter()
        .map(|file| {
            if file.lfs {
                json!({"rfilename": file.path, "size": file.bytes.len(), "blobId": "f".repeat(40), "lfs": {"sha256": file.sha256(), "size": file.bytes.len(), "pointerSize": 134}})
            } else {
                json!({"rfilename": file.path, "size": file.bytes.len(), "blobId": file.blob()})
            }
        })
        .collect();
    json!({
        "id": repo, "sha": REVISION, "downloads": 900, "likes": 4, "pipeline_tag": entry.pipeline,
        "gated": if entry.gated { json!("auto") } else { json!(false) },
        "cardData": {"license": "apache-2.0"}, "tags": entry.tags, "siblings": siblings,
    })
}

async fn serve(
    mut socket: TcpStream,
    repos: Arc<BTreeMap<&'static str, Repo>>,
    throttle: Arc<AtomicBool>,
    ranges: Arc<Mutex<Vec<String>>>,
    authorized: Arc<Mutex<Vec<(String, bool)>>>,
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
    let bearer = header("authorization").as_deref() == Some(&format!("Bearer {TOKEN}"));
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    authorized.lock().unwrap().push((path.to_owned(), bearer));
    let send = async |socket: &mut TcpStream, status: &str, headers: &str, body: &[u8]| {
        if socket
            .write_all(head(status, headers, body.len()).as_bytes())
            .await
            .is_ok()
        {
            let _ = socket.write_all(body).await;
        }
    };
    if path == "/api/models" {
        let items: Vec<Value> = repos
            .iter()
            .map(|(repo, entry)| json!({"id": repo, "downloads": 900, "likes": 4, "pipeline_tag": entry.pipeline, "tags": entry.tags}))
            .collect();
        let body = serde_json::to_vec(&items).unwrap();
        return send(
            &mut socket,
            "200 OK",
            "Content-Type: application/json\r\n",
            &body,
        )
        .await;
    }
    if let Some(rest) = path.strip_prefix("/api/models/") {
        let repo = rest.trim_end_matches(&format!("/revision/{REVISION}"));
        return match repos.get(repo) {
            Some(entry) if query.contains("blobs=true") => {
                let body = serde_json::to_vec(&info(repo, entry)).unwrap();
                send(
                    &mut socket,
                    "200 OK",
                    "Content-Type: application/json\r\n",
                    &body,
                )
                .await
            }
            _ => send(&mut socket, "404 Not Found", "", b"").await,
        };
    }
    let (repo, file, cached) = if let Some(rest) = path.strip_prefix("/api/resolve-cache/models/") {
        let mut parts = rest.splitn(4, '/');
        let (owner, name, revision, file) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        if revision != REVISION {
            return send(&mut socket, "404 Not Found", "", b"").await;
        }
        (format!("{owner}/{name}"), file.to_owned(), true)
    } else {
        let mut parts = path.trim_start_matches('/').splitn(5, '/');
        let (owner, name, resolve, revision, file) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        if resolve != "resolve" || revision != REVISION {
            return send(&mut socket, "404 Not Found", "", b"").await;
        }
        (format!("{owner}/{name}"), file.to_owned(), false)
    };
    let Some(entry) = repos.get(repo.as_str()) else {
        return send(&mut socket, "404 Not Found", "", b"").await;
    };
    let Some(found) = entry.files.iter().find(|known| known.path == file) else {
        return send(&mut socket, "404 Not Found", "", b"").await;
    };
    if entry.gated && !bearer {
        return send(&mut socket, "401 Unauthorized", "", b"").await;
    }
    if !found.lfs && !cached {
        let location = format!(
            "Location: /api/resolve-cache/models/{repo}/{REVISION}/{file}?etag=%22{}%22\r\n",
            found.blob()
        );
        return send(&mut socket, "307 Temporary Redirect", &location, b"").await;
    }
    let etag = format!(
        "\"{}\"",
        if found.lfs {
            found.sha256()
        } else {
            found.blob()
        }
    );
    let bytes = &found.bytes;
    let offset = header("range")
        .inspect(|range| ranges.lock().unwrap().push(range.clone()))
        .filter(|_| header("if-range").as_deref() == Some(etag.as_str()))
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
                "ETag: {etag}\r\nContent-Range: bytes {offset}-{}/{}\r\n",
                bytes.len() - 1,
                bytes.len()
            ),
            bytes.len() - offset,
        )
    } else {
        head("200 OK", &format!("ETag: {etag}\r\n"), bytes.len())
    };
    if socket.write_all(reply.as_bytes()).await.is_err() {
        return;
    }
    for chunk in bytes[offset..].chunks(16 * 1024) {
        if socket.write_all(chunk).await.is_err() {
            return;
        }
        if bytes.len() == WEIGHTS && throttle.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }
}

impl Hub {
    async fn start() -> Self {
        let repos = Arc::new(repos());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let throttle = Arc::new(AtomicBool::new(true));
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let authorized = Arc::new(Mutex::new(Vec::new()));
        let (served, slow, recorded, seen) = (
            repos.clone(),
            throttle.clone(),
            ranges.clone(),
            authorized.clone(),
        );
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(serve(
                    socket,
                    served.clone(),
                    slow.clone(),
                    recorded.clone(),
                    seen.clone(),
                ));
            }
        });
        Self {
            base,
            repos,
            throttle,
            ranges,
            authorized,
            task,
        }
    }

    fn file(&self, repo: &str, path: &str) -> &File {
        self.repos[repo]
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
    }
}

struct Github {
    base: String,
    task: JoinHandle<()>,
}

impl Drop for Github {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const ORT_REPOSITORY: &str = "lemonade-sdk/ort-server";
const ORT_TAG: &str = "0.3.7";
const ORT_ASSET: &str = "ort-server-0.3.7-linux-x64.tar.gz";

/// Serves an ort-server release whose entrypoint is the test worker.
async fn github() -> Github {
    let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (path, mode, data) in [
        ("ort-server-0.3.7/ort-server", 0o755, worker.as_slice()),
        ("ort-server-0.3.7/LICENSE", 0o644, b"MIT fixture".as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, data).unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    let archive = Arc::new(encoder.finish().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let archive = archive.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                while request.len() < 16384 && !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    request.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&request);
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, headers, body): (&str, String, Vec<u8>) = if path
                    == format!("/api/repos/{ORT_REPOSITORY}/releases/tags/{ORT_TAG}")
                {
                    let digest = format!("{:x}", Sha256::digest(archive.as_slice()));
                    let body = json!({"tag_name": ORT_TAG, "assets": [{"name": ORT_ASSET, "size": archive.len(), "digest": format!("sha256:{digest}")}]});
                    (
                        "200 OK",
                        "Content-Type: application/json\r\n".into(),
                        body.to_string().into_bytes(),
                    )
                } else if path
                    == format!("/{ORT_REPOSITORY}/releases/download/{ORT_TAG}/{ORT_ASSET}")
                {
                    (
                        "302 Found",
                        format!("Location: /objects/{ORT_ASSET}\r\n"),
                        Vec::new(),
                    )
                } else if path == format!("/objects/{ORT_ASSET}") {
                    ("200 OK", String::new(), archive.to_vec())
                } else {
                    ("404 Not Found", String::new(), Vec::new())
                };
                let _ = socket
                    .write_all(head(status, &headers, body.len()).as_bytes())
                    .await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    Github { base, task }
}

fn cpu_only(root: &Path) {
    let mut cpuinfo = String::new();
    for processor in 0..8 {
        cpuinfo.push_str(&format!(
            "processor\t: {processor}\nvendor_id\t: AuthenticAMD\nmodel name\t: Fixture CPU\nphysical id\t: 0\ncore id\t\t: {}\ncpu cores\t: 4\nflags\t\t: avx avx2 fma f16c\n\n",
            processor % 4
        ));
    }
    std::fs::create_dir_all(root.join("proc")).unwrap();
    std::fs::write(root.join("proc/cpuinfo"), cpuinfo).unwrap();
    std::fs::write(
        root.join("proc/meminfo"),
        "MemTotal:       32763640 kB\nMemAvailable:   26446104 kB\n",
    )
    .unwrap();
}

struct Fixture {
    server: Server,
    hub: Hub,
    _github: Github,
    _hardware: tempfile::TempDir,
}

impl std::ops::Deref for Fixture {
    type Target = Server;
    fn deref(&self) -> &Server {
        &self.server
    }
}

async fn fixture() -> Fixture {
    let hub = Hub::start().await;
    let github = github().await;
    let hardware = tempfile::tempdir().unwrap();
    cpu_only(hardware.path());
    let mut server = Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["resident_budget_bytes"] = json!(4u64 * 1024 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(10000);
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.set_environment("HF_TOKEN", TOKEN);
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.set_environment("JUICED_TEST_ENGINE_RELEASES", &github.base);
    server.start().await;
    Fixture {
        server,
        hub,
        _github: github,
        _hardware: hardware,
    }
}

async fn until(what: &str, mut condition: impl AsyncFnMut() -> Option<Value>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = condition().await {
            return value;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn entry<'a>(result: &'a Value, repo: &str) -> &'a Value {
    result["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["repo"] == repo)
        .unwrap_or_else(|| panic!("{repo} missing from {result}"))
}

async fn pull(server: &Server, body: Value) -> Value {
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let mut body = body;
    body["expected_generation"] = generation;
    let (status, view) = server.post("juiced/library/pull", body).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn settled(server: &Server, id: &str) -> Value {
    until("pull completion", async || {
        let view = server.get(&format!("juiced/library/pulls/{id}")).await;
        matches!(view["state"].as_str(), Some("complete" | "failed")).then_some(view)
    })
    .await
}

#[tokio::test]
async fn search_lists_checkpoint_folders_with_their_engine_files_and_filters() {
    let fixture = fixture().await;
    let result = fixture.get("juiced/library/search?q=Tiny").await;
    assert_eq!(result["sources"]["huggingface"]["status"], "ok", "{result}");
    assert_eq!(result["sources"]["huggingface"]["authenticated"], true);
    let chat = entry(&result, CHAT);
    let [checkpoint] = chat["quants"].as_array().unwrap().as_slice() else {
        panic!("one checkpoint variant expected: {chat}");
    };
    assert_eq!(checkpoint["kind"], "checkpoint");
    assert_eq!(checkpoint["format"], "transformers");
    assert_eq!(checkpoint["engine"], "vllm");
    assert_eq!(checkpoint["file"], "config.json");
    assert_eq!(checkpoint["pullable"], true, "{checkpoint}");
    let files: Vec<&str> = checkpoint["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        [
            "config.json",
            "model.safetensors",
            "generation_config.json",
            "tokenizer_config.json",
            "tokenizer.json"
        ]
    );
    let expected: usize = fixture.hub.repos[CHAT]
        .files
        .iter()
        .filter(|file| files.contains(&file.path))
        .map(|file| file.bytes.len())
        .sum();
    assert_eq!(checkpoint["bytes"], expected);
    assert_eq!(chat["license"], "apache-2.0");
    assert_eq!(chat["gated"], false);
    let classifier = entry(&result, CLASSIFIER);
    assert_eq!(classifier["gated"], true);
    assert_eq!(classifier["quants"][0]["label"], "ONNX");
    assert_eq!(classifier["quants"][0]["engine"], "onnxruntime");
    assert_eq!(
        classifier["quants"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["member"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["model.onnx", "tokenizer.json", "config.json", "vocab.txt"]
    );

    let onnx = fixture
        .get("juiced/library/search?q=Tiny&engine=onnxruntime")
        .await;
    assert_eq!(onnx["engine"], "onnxruntime");
    let repos: Vec<&str> = onnx["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["repo"].as_str().unwrap())
        .collect();
    assert_eq!(repos, [CLASSIFIER]);
    let classify = fixture
        .get("juiced/library/search?q=Tiny&task=classify")
        .await;
    assert!(
        classify["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["repo"] == CLASSIFIER),
        "{classify}"
    );
    let (status, refused) = fixture
        .json(
            Method::GET,
            "/v1/juiced/library/search?engine=nope",
            None,
            Some(support::ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert_eq!(refused["error"]["code"], "invalid_library_query");
}

#[tokio::test]
async fn transformers_pull_learns_small_file_digests_resumes_after_a_crash_and_detects_vllm() {
    let mut fixture = fixture().await;
    let started = pull(
        &fixture,
        json!({"repo": CHAT, "files": ["config.json", "model.safetensors", "tokenizer.json"]}),
    )
    .await;
    assert_eq!(started["format"], "transformers");
    assert_eq!(started["engine"], "vllm");
    assert_eq!(started["suggested_id"], "Tiny-Transformers");
    assert_eq!(started["label"], "Checkpoint");
    assert_eq!(started["files"].as_array().unwrap().len(), 5);
    let config = fixture.hub.file(CHAT, "config.json");
    assert_eq!(started["files"][0]["sha256"], config.sha256());
    assert_eq!(started["files"][0]["member"], "config.json");
    let id = started["pull"].as_str().unwrap().to_owned();
    let weights_job = until("weights transfer", async || {
        let view = fixture.get(&format!("juiced/library/pulls/{id}")).await;
        let weights = view["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["file"] == "model.safetensors")
            .cloned()?;
        let job = fixture
            .get(&format!(
                "juiced/artifacts/jobs/{}",
                weights["job"].as_str()?
            ))
            .await;
        let done = job["downloaded_bytes"].as_u64().unwrap_or(0);
        (done > (WEIGHTS / 8) as u64 && done < (WEIGHTS / 2) as u64).then_some(job)
    })
    .await;
    fixture.server.crash();
    let journal: Value = serde_json::from_slice(
        &std::fs::read(fixture.directory.path().join("acquisitions.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        journal["pulls"][&id]["checkpoint"], "transformers",
        "{journal}"
    );
    fixture.hub.throttle.store(false, Ordering::Release);
    fixture.server.start().await;
    let done = settled(&fixture, &id).await;
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(done["set_state"], "published");
    assert_eq!(done["engine"], "vllm");
    assert!(
        fixture
            .hub
            .ranges
            .lock()
            .unwrap()
            .iter()
            .any(|range| range != "bytes=0-"),
        "the restart should resume {}",
        weights_job["id"]
    );
    let set = done["set"].as_str().unwrap().to_owned();
    let view = fixture.get(&format!("juiced/artifacts/sets/{set}")).await;
    let mut members: Vec<(&str, &str)> = view["specification"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| {
            (
                member["name"].as_str().unwrap(),
                member["role"].as_str().unwrap(),
            )
        })
        .collect();
    members.sort();
    assert_eq!(
        members,
        [
            ("config.json", "model"),
            ("generation_config.json", "configuration"),
            ("model.safetensors", "external-tensor"),
            ("tokenizer.json", "tokenizer"),
            ("tokenizer_config.json", "tokenizer"),
        ],
        "{view}"
    );
    let searched = fixture.get("juiced/library/search?q=Tiny").await;
    let local = &entry(&searched, CHAT)["quants"][0]["local"];
    assert_eq!(local["present"], true, "{local}");
    assert_eq!(local["set"], set);
    assert_eq!(local["storage"], "artifact_store");

    let (status, refused) = fixture
        .post("juiced/models/add", json!({"artifact_set": set}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "no_usable_backend", "{refused}");

    let (status, variants) = fixture
        .json(
            Method::GET,
            &format!("/api/v1/pull/variants?checkpoint={CHAT}"),
            None,
            Some(support::ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{variants}");
    assert_eq!(variants["recipe"], "vllm");
    assert_eq!(variants["variants"][0]["name"], "Checkpoint");
    let (status, native) = fixture
        .json(
            Method::POST,
            "/api/v1/pull",
            Some(json!({"model_name": "user.Tiny-Chat", "checkpoint": CHAT, "recipe": "vllm"})),
            Some(support::ADMIN_KEY),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{native}");
    assert!(
        native["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("vllm")
                || message.contains("engine")
                || message.contains("backend")),
        "the native pull reached registration: {native}"
    );
}

#[tokio::test]
async fn gated_onnx_classifier_pulls_with_the_token_registers_on_ort_and_classifies() {
    let fixture = fixture().await;
    let started = pull(&fixture, json!({"repo": CLASSIFIER, "file": "model.onnx"})).await;
    let id = started["pull"].as_str().unwrap();
    let done = settled(&fixture, id).await;
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(done["format"], "onnx-classifier");
    for (path, authorized) in fixture.hub.authorized.lock().unwrap().iter() {
        if path.contains(CLASSIFIER) && !path.starts_with("/api/models") {
            assert!(authorized, "{path} was fetched without the hub token");
        }
    }
    let set = done["set"].as_str().unwrap().to_owned();

    let (status, accepted) = fixture
        .post(
            "juiced/engines/install",
            json!({"engine": "onnxruntime", "backend": "cpu", "expected_generation": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let job = accepted["job"]["id"].as_str().unwrap().to_owned();
    until("engine install", async || {
        let engines = fixture.get("juiced/engines").await;
        engines["jobs"]
            .as_array()?
            .iter()
            .find(|row| row["id"] == job.as_str() && row["state"] == "complete")
            .cloned()
    })
    .await;

    let (status, added) = fixture
        .post(
            "juiced/models/add",
            json!({"artifact_set": set, "policy": "on-demand"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["engine"], "onnxruntime");
    assert_eq!(added["format"]["format"], "onnx-classifier");
    assert_eq!(added["format"]["labels"], 2);
    assert_eq!(added["model"]["id"], "Tiny-Classifier-ONNX");
    assert_eq!(added["artifact_set"], set);
    assert_eq!(
        added["execution_profile"]["args"],
        json!(["--model-path", "{model}", "--port", "{port}"])
    );
    let (status, reply) = fixture
        .json(
            Method::POST,
            "/v1/classify",
            Some(json!({"model": "Tiny-Classifier-ONNX", "input": "Please verify your account now."})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["object"], "classification");

    let storage = fixture.get("juiced/storage").await;
    let text = storage.to_string();
    assert!(text.contains("Tiny-Classifier-ONNX"), "{storage}");
    let (status, removed) = fixture
        .json(
            Method::POST,
            "/api/v1/delete",
            Some(json!({"model_name": "Tiny-Classifier-ONNX"})),
            Some(support::ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    let onnx = fixture.hub.file(CLASSIFIER, "model.onnx").sha256();
    assert_eq!(removed["weights_removed"], true, "{removed}");
    let inventory = fixture.get("juiced/artifacts").await;
    assert!(
        !inventory["artifacts"].to_string().contains(&onnx)
            && inventory["artifact_sets"].as_array().unwrap().is_empty(),
        "deleting the model frees its checkpoint files: {inventory}"
    );
}
