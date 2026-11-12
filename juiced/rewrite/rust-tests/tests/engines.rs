#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{ADMIN_KEY, CLIENT_KEY, PREFIXES, Server, binary, metadata_gguf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const LLAMA_CPU_REPOSITORY: &str = "ggml-org/llama.cpp";
const LLAMA_CPU_TAG: &str = "b12032";
const LLAMA_CPU_ASSET: &str = "llama-b12032-bin-ubuntu-x64.tar.gz";
const WHISPER_REPOSITORY: &str = "lemonade-sdk/whisper.cpp-rocm";
const WHISPER_TAG: &str = "v1.8.4";
const WHISPER_ASSET: &str = "whisper-v1.8.4-linux-cpu-x86_64.tar.gz";

struct Release {
    published_sha256: String,
    bytes: Vec<u8>,
}

struct FakeGithub {
    base: String,
    downloads: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

async fn fake_github(
    releases: BTreeMap<(String, String, String), Release>,
    weights: Vec<u8>,
) -> FakeGithub {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let releases = Arc::new(releases);
    let weights = Arc::new(weights);
    let downloads = Arc::new(AtomicUsize::new(0));
    let counter = downloads.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let releases = releases.clone();
            let weights = weights.clone();
            let counter = counter.clone();
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
                assert!(!text.to_ascii_lowercase().contains("authorization:"));
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let reply = if path == "/weights/fixture-model.gguf" {
                    response("200 OK", "", &weights)
                } else if let Some(rest) = path.strip_prefix("/api/repos/") {
                    let found = releases.iter().find(|((repository, tag, _), _)| {
                        rest == format!("{repository}/releases/tags/{tag}")
                    });
                    match found {
                        Some(((repository, tag, _), _)) => {
                            let assets: Vec<_> = releases
                                .iter()
                                .filter(|((r, t, _), _)| r == repository && t == tag)
                                .map(|((_, _, asset), release)| {
                                    json!({"name":asset,"size":release.bytes.len(),"digest":format!("sha256:{}", release.published_sha256)})
                                })
                                .collect();
                            response(
                                "200 OK",
                                "Content-Type: application/json\r\n",
                                json!({"tag_name":tag,"assets":assets})
                                    .to_string()
                                    .as_bytes(),
                            )
                        }
                        None => response("404 Not Found", "", b"{}"),
                    }
                } else if let Some(asset) = path.strip_prefix("/objects/") {
                    match releases.iter().find(|((_, _, name), _)| name == asset) {
                        Some((_, release)) => {
                            counter.fetch_add(1, Ordering::AcqRel);
                            response("200 OK", "", &release.bytes)
                        }
                        None => response("404 Not Found", "", b""),
                    }
                } else if releases.keys().any(|(repository, tag, asset)| {
                    path == format!("/{repository}/releases/download/{tag}/{asset}")
                }) {
                    let asset = path.rsplit('/').next().unwrap();
                    response("302 Found", &format!("Location: /objects/{asset}\r\n"), b"")
                } else {
                    response("404 Not Found", "", b"")
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    FakeGithub {
        base,
        downloads,
        task,
    }
}

fn release_archive(top: &str, server: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut append = |path: String, mode: u32, data: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, data).unwrap();
    };
    append(format!("{top}/llama-server"), 0o755, server);
    append(
        format!("{top}/libggml.so.0.25.3"),
        0o755,
        b"fixture library bytes",
    );
    append(format!("{top}/LICENSE"), 0o644, b"MIT fixture");
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    builder
        .append_link(
            &mut header,
            format!("{top}/libggml.so.0"),
            "libggml.so.0.25.3",
        )
        .unwrap();
    let tar = builder.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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

struct EngineServer {
    server: Server,
    _hardware: tempfile::TempDir,
    github: FakeGithub,
    weights: Vec<u8>,
}

impl std::ops::Deref for EngineServer {
    type Target = Server;
    fn deref(&self) -> &Server {
        &self.server
    }
}

impl Drop for EngineServer {
    fn drop(&mut self) {
        self.server.stop();
        fn writable(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                            writable(&entry.path());
                        }
                    }
                }
            }
        }
        writable(&self.server.directory.path().join("runtime-packages"));
    }
}

async fn engine_server() -> EngineServer {
    engine_server_with(|_| {}).await
}

async fn engine_server_with(edit: impl FnOnce(&mut Value)) -> EngineServer {
    let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
    let llama = release_archive("llama-b12032", &worker);
    let whisper = release_archive("whisper-v1.8.4-linux-cpu-x86_64", b"not published");
    let mut releases = BTreeMap::new();
    releases.insert(
        (
            LLAMA_CPU_REPOSITORY.to_owned(),
            LLAMA_CPU_TAG.to_owned(),
            LLAMA_CPU_ASSET.to_owned(),
        ),
        Release {
            published_sha256: digest(&llama),
            bytes: llama,
        },
    );
    releases.insert(
        (
            WHISPER_REPOSITORY.to_owned(),
            WHISPER_TAG.to_owned(),
            WHISPER_ASSET.to_owned(),
        ),
        Release {
            published_sha256: digest(b"different bytes were published"),
            bytes: whisper,
        },
    );
    let weights = metadata_gguf(2048);
    let github = fake_github(releases, weights.clone()).await;
    let hardware = tempfile::tempdir().unwrap();
    cpu_only(hardware.path());
    let mut server = Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["resident_budget_bytes"] = json!(4u64 * 1024 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(10000);
        edit(config);
    });
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.set_environment("JUICED_TEST_ENGINE_RELEASES", &github.base);
    server.start().await;
    EngineServer {
        server,
        _hardware: hardware,
        github,
        weights,
    }
}

async fn engines(server: &Server, prefix: &str) -> Value {
    let (status, value) = server
        .json(
            Method::GET,
            &format!("{prefix}/juiced/engines"),
            None,
            Some(ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn wait_for_job(server: &Server, id: &str) -> Value {
    for _ in 0..600 {
        let document = engines(server, "/v1").await;
        let job = document["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .cloned()
            .expect("retained job");
        if job["state"] == "complete" || job["state"] == "failed" {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("engine installation did not settle");
}

async fn pull_weights(server: &EngineServer) -> String {
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let (status, job) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":{
                "source_url":format!("{}/weights/fixture-model.gguf", server.github.base),
                "expected_sha256":digest(&server.weights),"expected_bytes":server.weights.len(),
                "maximum_bytes":server.weights.len()}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let job = server.get(&format!("juiced/artifacts/jobs/{id}")).await;
        if job["state"] == "complete" {
            return job["artifact"].as_str().unwrap().to_owned();
        }
        assert_ne!(job["state"], "failed", "{job}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("weights did not download");
}

fn row<'a>(document: &'a Value, engine: &str, backend: &str) -> &'a Value {
    document["installable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["engine"] == engine && row["backend"] == backend)
        .unwrap_or_else(|| panic!("missing {engine}:{backend}"))
}

#[tokio::test]
async fn engine_install_auto_registration_load_and_chat_use_the_installed_package() {
    let server = engine_server().await;
    for prefix in PREFIXES {
        let path = format!("{prefix}/juiced/engines");
        for key in [None, Some(CLIENT_KEY), Some("invalid-key")] {
            let response = server.request(Method::GET, &path, None, key).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        for (route, body) in [
            (
                "/juiced/engines/install",
                json!({"engine":"llama.cpp","backend":"cpu","expected_generation":0}),
            ),
            ("/juiced/models/add", json!({"artifact":"x"})),
        ] {
            let response = server
                .request(
                    Method::POST,
                    &format!("{prefix}{route}"),
                    Some(body),
                    Some(CLIENT_KEY),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{prefix}{route}"
            );
        }
        let document = engines(&server, prefix).await;
        assert_eq!(document["generation"], 0);
        assert_eq!(document["recommendations"]["llamacpp"], "cpu");
        let cpu = row(&document, "llama.cpp", "cpu");
        assert_eq!(cpu["availability"], "available");
        assert_eq!(cpu["recommended"], true);
        assert_eq!(cpu["build"]["asset"], LLAMA_CPU_ASSET);
        assert_eq!(cpu["build"]["repository"], LLAMA_CPU_REPOSITORY);
        assert_eq!(cpu["installed_package"], Value::Null);
        let cuda = row(&document, "llama.cpp", "cuda");
        assert_eq!(cuda["build"], Value::Null);
        assert_eq!(cuda["unresolved"], "no_matching_gpu");
    }
    let artifact = pull_weights(&server).await;
    let (status, refused) = server
        .post("juiced/models/add", json!({"artifact":artifact}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "engine_not_installed");
    assert_eq!(refused["install"]["engine"], "llama.cpp");
    assert_eq!(refused["install"]["backend"], "cpu");
    assert_eq!(refused["install"]["installable"], true);
    assert!(
        server.get("juiced/catalog").await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(server.github.downloads.load(Ordering::Acquire), 0);

    for (body, code) in [
        (
            json!({"engine":"llama.cpp","backend":"cpu","expected_generation":7}),
            "manifest_revision_conflict",
        ),
        (
            json!({"engine":"llama.cpp","backend":"cuda","expected_generation":0}),
            "engine_build_unavailable",
        ),
        (
            json!({"engine":"llama.cpp","backend":"hybrid-rocm","expected_generation":0}),
            "engine_build_unavailable",
        ),
        (
            json!({"engine":"llama.cpp","backend":"metal","expected_generation":0}),
            "unknown_backend",
        ),
        (
            json!({"engine":"vllm","backend":"cpu","expected_generation":0}),
            "unknown_engine",
        ),
    ] {
        let (status, error) = server.post("juiced/engines/install", body).await;
        assert!(status.is_client_error(), "{error}");
        assert_eq!(error["error"]["code"], code);
    }
    let (status, accepted) = server
        .json(
            Method::POST,
            "/api/v1/juiced/engines/install",
            Some(json!({"engine":"llama.cpp","backend":"cpu","expected_generation":0})),
            Some(ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let job = wait_for_job(&server, accepted["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "complete", "{job}");
    assert_eq!(job["digest_source"], "github-release-api");
    assert_eq!(job["asset"], LLAMA_CPU_ASSET);
    let package = job["package"].as_str().unwrap().to_owned();
    assert!(!server.directory.path().join("engine-staging").exists());

    let document = engines(&server, "/v0").await;
    assert_eq!(document["generation"], 2);
    assert_eq!(
        row(&document, "llama.cpp", "cpu")["installed_package"],
        package
    );
    let installed = &document["installed"][0];
    assert_eq!(installed["package"], package);
    assert_eq!(installed["engine"], "llama.cpp");
    assert_eq!(installed["release"]["repository"], LLAMA_CPU_REPOSITORY);
    assert_eq!(installed["release"]["tag"], LLAMA_CPU_TAG);
    assert_eq!(installed["release"]["asset"], LLAMA_CPU_ASSET);
    assert_eq!(installed["release"]["backend"], "cpu");
    assert_eq!(
        installed["declared_target"]["compute_paths"],
        json!(["cpu"])
    );
    assert_eq!(installed["models"], json!([]));
    let payload = server
        .directory
        .path()
        .join("runtime-packages")
        .join(&package)
        .join("payload");
    let mut files: Vec<_> = std::fs::read_dir(&payload)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["LICENSE", "libggml.so.0", "llama-server"]);

    let (status, again) = server
        .post(
            "juiced/engines/install",
            json!({"engine":"llama.cpp","backend":"cpu","expected_generation":2}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["already_installed"], true);
    assert_eq!(again["package"], package);
    assert_eq!(server.github.downloads.load(Ordering::Acquire), 1);

    let mut ids = Vec::new();
    for (index, prefix) in PREFIXES.into_iter().enumerate() {
        let id = format!("auto-{index}");
        let mut body = json!({"artifact":artifact,"id":id});
        if index == 0 {
            body["policy"] = json!("on-demand");
        }
        let (status, added) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/models/add"),
                Some(body),
                Some(ADMIN_KEY),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{added}");
        assert_eq!(added["engine"], "llama.cpp");
        assert_eq!(added["backend"], "cpu");
        assert_eq!(added["package"], package);
        assert!(added["reason"].as_str().unwrap().contains("recommended"));
        assert_eq!(added["format"]["format"], "gguf");
        assert_eq!(added["format"]["declared"]["training_context_tokens"], 2048);
        assert_eq!(
            added["model"]["policy"],
            if index == 0 { "on-demand" } else { "disabled" }
        );
        let profile = &added["execution_profile"];
        assert_eq!(profile["runtime_package"], package);
        assert_eq!(profile["options"]["context_tokens"], 2048);
        assert_eq!(profile["options"]["gpu_layers"], 0);
        assert_eq!(profile["options"]["parallel_slots"], 1);
        assert_eq!(profile["options"]["threads"], 4);
        let estimate = &added["estimate"];
        assert_eq!(estimate["basis"], "file-fraction");
        assert_eq!(
            estimate["resident_bytes"],
            server.weights.len() as u64 + server.weights.len() as u64 / 4 + 256 * 1024 * 1024
        );
        ids.push(id);
    }
    let (status, duplicate) = server
        .post(
            "juiced/models/add",
            json!({"artifact":artifact,"id":"auto-0"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(duplicate["error"]["code"], "model_exists");
    let (status, missing) = server.post("juiced/models/add", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(missing["error"]["code"], "artifact_required");
    let (status, hybrid) = server
        .post(
            "juiced/models/add",
            json!({"artifact":artifact,"id":"hybrid","backend":"hybrid-rocm"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(hybrid["error"]["code"], "backend_not_selectable");

    let document = engines(&server, "/api/v0").await;
    let models: Vec<_> = document["installed"][0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            assert_eq!(entry["selection"], "current");
            entry["model"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(models, ids);

    let (status, loaded) = server.post("load", json!({"model":"auto-0"})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["runtime_package"], package);
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(
                json!({"model":"auto-0","messages":[{"role":"user","content":"engine pipeline"}]}),
            ),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], "auto-0");
    let (status, _) = server.post("unload", json!({"model":"auto-0"})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, accepted) = server
        .json(
            Method::POST,
            "/v0/juiced/engines/install",
            Some(json!({"engine":"whisper.cpp","backend":"cpu","expected_generation":2})),
            Some(ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let failed = wait_for_job(&server, accepted["job"]["id"].as_str().unwrap()).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error"], "engine_hash_mismatch");
    assert_eq!(failed["package"], Value::Null);
    let document = engines(&server, "/v1").await;
    assert_eq!(document["generation"], 2);
    assert_eq!(document["installed"].as_array().unwrap().len(), 1);
    assert!(!server.directory.path().join("engine-staging").exists());
}

async fn chat(server: &Server, model: &str) {
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":model,"messages":[{"role":"user","content":"local file"}]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["model"], model);
}

#[tokio::test]
async fn uploaded_and_linked_local_files_use_the_automatic_add_flow() {
    let imports = tempfile::tempdir().unwrap();
    let root = imports.path().to_owned();
    let server = engine_server_with(|config| config["import_roots"] = json!([root])).await;
    let upload = format!(
        "{}/v1/juiced/library/upload?filename=local%20fixture.gguf&size={}",
        server.base,
        server.weights.len()
    );
    let uploaded: Value = server
        .client
        .post(&upload)
        .bearer_auth(ADMIN_KEY)
        .header("content-type", "application/octet-stream")
        .body(server.weights.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(uploaded["sha256"], digest(&server.weights), "{uploaded}");
    assert_eq!(uploaded["suggested_id"], "local fixture");
    let option = &uploaded["engines"][0];
    assert_eq!(option["engine"], "llama.cpp");
    assert_eq!(option["ready"], false);
    assert_eq!(option["error"], "engine_not_installed");
    assert_eq!(option["install"]["backend"], "cpu");
    assert_eq!(option["installed_backends"], json!([]));

    let (status, accepted) = server
        .post(
            "juiced/engines/install",
            json!({"engine":"llama.cpp","backend":"cpu","expected_generation":0}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let job = wait_for_job(&server, accepted["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "complete", "{job}");

    let (status, added) = server
        .post(
            "juiced/models/add",
            json!({"artifact":uploaded["add"]["artifact"],"id":uploaded["add"]["id"],"policy":"on-demand"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["backend"], "cpu");
    assert_eq!(added["model"]["policy"], "on-demand");
    chat(&server, "local fixture").await;

    let linked_file = root.join("linked.gguf");
    std::fs::write(&linked_file, &server.weights).unwrap();
    let (status, linked) = server
        .post(
            "juiced/library/import-path",
            json!({"path":linked_file,"mode":"link"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["mode"], "link");
    assert_eq!(linked["job"], Value::Null);
    assert_eq!(linked["engines"][0]["ready"], true);
    assert_eq!(linked["engines"][0]["backend"], "cpu");
    assert_eq!(linked["engines"][0]["installed_backends"], json!(["cpu"]));
    let (status, refused) = server
        .post(
            "juiced/models/add",
            json!({"path":linked_file,"artifact":uploaded["artifact"]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"]["code"], "model_source_ambiguous");
    let (status, added) = server
        .post(
            "juiced/models/add",
            json!({"path":linked["add"]["path"],"id":linked["add"]["id"],"policy":"on-demand"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["model"]["artifact"], Value::Null);
    chat(&server, "linked").await;
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    let model = saved["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "linked")
        .unwrap();
    assert_eq!(
        model["model_file"],
        json!(std::fs::canonicalize(&linked_file).unwrap())
    );
    assert_eq!(model.get("artifact"), None);
    assert!(linked_file.is_file());
}
