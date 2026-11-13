#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Fake-hardware process tests for the AMD NPU engines. The sysfs tree, release server
//! and engine executables are fixtures: these tests prove detection, installation, launch
//! arguments, environment and request routing, never NPU execution.
mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::Path, sync::Arc, time::Duration};
use support::{CLIENT_KEY, PREFIXES, Server, binary, metadata_gguf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const FLM_REPOSITORY: &str = "ROCm/FastFlowLM";
const FLM_TAG: &str = "v1.0.12";
const FLM_ASSET: &str = "fastflowlm_1.0.12_linux.tar.gz";
const HYBRID_REPOSITORY: &str = "Nulifyer/llama-xdna-hybrid";
const HYBRID_TAG: &str = "rocm-v0.3.2";
const HYBRID_ASSET: &str = "llama-xdna-hybrid-linux-rocm-gfx1150-x86_64.tar.gz";

type Releases = BTreeMap<(String, String), Vec<u8>>;

fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Serves `/api/repos/<repo>/releases/tags/<tag>` digests and the release assets.
async fn fake_github(
    releases: Releases,
    weights: Vec<u8>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let releases = Arc::new(releases);
    let weights = Arc::new(weights);
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let releases = releases.clone();
            let weights = weights.clone();
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
                let reply = if path == "/weights/fixture-model.gguf" {
                    response("200 OK", "", &weights)
                } else if let Some(rest) = path.strip_prefix("/api/repos/") {
                    let assets: Vec<_> = releases
                        .iter()
                        .filter(|((repository, _), _)| {
                            rest.starts_with(&format!("{repository}/releases/tags/"))
                        })
                        .map(|((_, asset), bytes)| {
                            json!({"name":asset,"size":bytes.len(),"digest":format!("sha256:{}", digest(bytes))})
                        })
                        .collect();
                    response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        json!({"assets":assets}).to_string().as_bytes(),
                    )
                } else if let Some(((_, _), bytes)) =
                    releases.iter().find(|((repository, asset), _)| {
                        path.starts_with(&format!("/{repository}/releases/download/"))
                            && path.ends_with(&format!("/{asset}"))
                    })
                {
                    response("200 OK", "", bytes)
                } else {
                    response("404 Not Found", "", b"")
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    (base, task)
}

enum Entry<'a> {
    File(&'a str, u32, Vec<u8>),
    Link(&'a str, &'a str),
}

fn archive(entries: Vec<Entry<'_>>) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        match entry {
            Entry::File(path, mode, data) => {
                header.set_size(data.len() as u64);
                header.set_mode(mode);
                header.set_entry_type(tar::EntryType::Regular);
                builder
                    .append_data(&mut header, path, data.as_slice())
                    .unwrap();
            }
            Entry::Link(path, target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                builder.append_link(&mut header, path, target).unwrap();
            }
        }
    }
    let tar = builder.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Ryzen AI 9 HX 370: XDNA 2 NPU and a gfx1150 Radeon 890M iGPU.
fn strix_point(root: &Path, memlock: &str) {
    let mut cpuinfo = String::new();
    for processor in 0..8 {
        cpuinfo.push_str(&format!(
            "processor\t: {processor}\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen AI 9 HX 370 w/ Radeon 890M\nphysical id\t: 0\ncore id\t\t: {}\ncpu cores\t: 4\nflags\t\t: avx avx2 fma f16c avx512f\n\n",
            processor % 4
        ));
    }
    write(root, "proc/cpuinfo", &cpuinfo);
    write(
        root,
        "proc/meminfo",
        "MemTotal:       32763640 kB\nMemAvailable:   26446104 kB\n",
    );
    write(root, "sys/class/drm/card1/device/vendor", "0x1002\n");
    write(root, "sys/class/drm/card1/device/device", "0x150e\n");
    write(
        root,
        "sys/class/drm/card1/device/uevent",
        "DRIVER=amdgpu\nPCI_ID=1002:150E\nPCI_SLOT_NAME=0000:c4:00.0\n",
    );
    std::fs::create_dir_all(root.join("sys/class/drm/card1/device/drm/renderD128")).unwrap();
    write(root, "dev/dri/renderD128", "");
    write(root, "dev/kfd", "");
    write(
        root,
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "simd_count 32\ngfx_target_version 110500\nvendor_id 4098\ndevice_id 5390\ndrm_render_minor 128\n",
    );
    write(
        root,
        "sys/class/accel/accel0/device/uevent",
        "DRIVER=amdxdna\nPCI_ID=1022:17F0\nPCI_SLOT_NAME=0000:c5:00.1\n",
    );
    write(root, "sys/class/accel/accel0/device/vendor", "0x1022\n");
    write(root, "sys/class/accel/accel0/device/device", "0x17f0\n");
    write(
        root,
        "sys/class/accel/accel0/device/fw_version",
        "1.5.5.391\n",
    );
    write(root, "sys/module/amdxdna/version", "2.21.0\n");
    write(root, "dev/accel/accel0", "");
    write(root, "limits/memlock", memlock);
}

struct NpuServer {
    server: Server,
    _hardware: tempfile::TempDir,
    _github: tokio::task::JoinHandle<()>,
    github: String,
    weights: Vec<u8>,
    record: std::path::PathBuf,
}

impl std::ops::Deref for NpuServer {
    type Target = Server;
    fn deref(&self) -> &Server {
        &self.server
    }
}

impl Drop for NpuServer {
    fn drop(&mut self) {
        self.server.stop();
        self._github.abort();
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

async fn npu_server(memlock: &str) -> NpuServer {
    let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
    let hardware = tempfile::tempdir().unwrap();
    strix_point(hardware.path(), memlock);
    let record = hardware.path().join("launches.log");
    let flm = archive(vec![
        Entry::File(
            "fastflowlm/flm",
            0o755,
            b"#!/bin/bash\nexec flm-real \"$@\"\n".to_vec(),
        ),
        Entry::File("fastflowlm/flm-real", 0o755, worker.clone()),
        Entry::File(
            "fastflowlm/lib/libxrt_core.so.2.25.00",
            0o755,
            b"xrt core".to_vec(),
        ),
        Entry::Link("fastflowlm/lib/libxrt_core.so.2", "libxrt_core.so.2.25.00"),
        Entry::Link(
            "fastflowlm/lib/x86_64-linux-gnu/libxrt_core.so.2",
            "../libxrt_core.so.2.25.00",
        ),
        Entry::File("fastflowlm/LICENSE_BINARY.txt", 0o644, b"fixture".to_vec()),
    ]);
    let hybrid = archive(vec![
        Entry::File(
            "llama-xdna-hybrid/hybrid-server",
            0o755,
            b"#!/usr/bin/env bash\nexec llama-server \"$@\"\n".to_vec(),
        ),
        Entry::File("llama-xdna-hybrid/llama-server", 0o755, worker),
        Entry::File(
            "llama-xdna-hybrid/libggml-xdna.so",
            0o755,
            b"plugin".to_vec(),
        ),
        Entry::Link(
            "llama-xdna-hybrid/rocm/lib/rocm_sysdeps/include/netlink",
            "libnl3/netlink",
        ),
        Entry::File(
            "llama-xdna-hybrid/rocm/lib/rocm_sysdeps/include/libnl3/netlink/netlink.h",
            0o644,
            b"/* header */".to_vec(),
        ),
    ]);
    let mut releases = BTreeMap::new();
    releases.insert((FLM_REPOSITORY.to_owned(), FLM_ASSET.to_owned()), flm);
    releases.insert(
        (HYBRID_REPOSITORY.to_owned(), HYBRID_ASSET.to_owned()),
        hybrid,
    );
    let weights = metadata_gguf(2048);
    let (github, task) = fake_github(releases, weights.clone()).await;
    let mut server = Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["resident_budget_bytes"] = json!(8u64 * 1024 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(10000);
    });
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.set_environment("JUICED_TEST_ENGINE_RELEASES", &github);
    server.set_environment("FIXTURE_LAUNCH_RECORD", &record);
    server.start().await;
    NpuServer {
        server,
        _hardware: hardware,
        _github: task,
        github,
        weights,
        record,
    }
}

async fn engines(server: &Server) -> Value {
    let (status, value) = server
        .json(Method::GET, "/v1/juiced/engines", None, server.admin_key())
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

fn row<'a>(document: &'a Value, engine: &str, backend: &str) -> &'a Value {
    document["installable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["engine"] == engine && row["backend"] == backend)
        .unwrap_or_else(|| panic!("missing {engine}:{backend}"))
}

fn variant<'a>(info: &'a Value, recipe: &str, backend: &str) -> &'a Value {
    info["backends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["recipe"] == recipe && row["backend"] == backend)
        .unwrap_or_else(|| panic!("missing {recipe}:{backend}"))
}

async fn install(server: &Server, engine: &str, backend: &str) -> String {
    let generation = engines(server).await["generation"].clone();
    let (status, accepted) = server
        .post(
            "juiced/engines/install",
            json!({"engine":engine,"backend":backend,"expected_generation":generation}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let id = accepted["job"]["id"].as_str().unwrap().to_owned();
    for _ in 0..600 {
        let document = engines(server).await;
        let job = document["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .cloned()
            .unwrap();
        if job["state"] == "complete" {
            return job["package"].as_str().unwrap().to_owned();
        }
        assert_ne!(job["state"], "failed", "{job}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("engine installation did not settle");
}

fn launches(record: &Path) -> String {
    std::fs::read_to_string(record).unwrap_or_default()
}

#[tokio::test]
async fn flm_install_pull_register_load_and_chat_on_a_fixture_npu_host() {
    let server = npu_server("unlimited\n").await;
    for prefix in PREFIXES {
        for (method, route, body) in [
            (Method::GET, "/juiced/flm", None),
            (
                Method::POST,
                "/juiced/flm/pull",
                Some(json!({"checkpoint":"gemma3:1b"})),
            ),
            (
                Method::POST,
                "/juiced/flm/models/add",
                Some(json!({"checkpoint":"gemma3:1b"})),
            ),
        ] {
            for key in [None, Some(CLIENT_KEY)] {
                let response = server
                    .request(
                        method.clone(),
                        &format!("{prefix}{route}"),
                        body.clone(),
                        key,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{prefix}{route}"
                );
            }
        }
    }
    let info = server.get("system-info").await;
    let npu = &info["devices"]["npus"][0];
    assert_eq!(npu["driver"], "amdxdna");
    assert_eq!(npu["family"], "XDNA2");
    assert_eq!(npu["access"], "read_write");
    assert_eq!(npu["firmware_version"], "1.5.5.391");
    assert_eq!(npu["driver_version"], "2.21.0");
    assert_eq!(info["devices"]["limits"]["memlock"], "unlimited");
    assert_eq!(info["devices"]["gpus"][0]["gfx_target"], "gfx1150");
    assert_eq!(variant(&info, "flm", "npu")["state"], "available");
    assert_eq!(info["recommendations"]["flm"], "npu");

    let status = server.get("juiced/flm").await;
    assert_eq!(status["available"], false);
    assert_eq!(status["unavailable"]["code"], "flm_not_installed");
    let (code, refused) = server
        .post("juiced/flm/pull", json!({"checkpoint":"gemma3:1b"}))
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "flm_not_installed");

    let document = engines(&server).await;
    assert_eq!(document["recommendations"]["flm"], "npu");
    let flm_row = row(&document, "fastflowlm", "npu");
    assert_eq!(flm_row["availability"], "available");
    assert_eq!(flm_row["build"]["repository"], FLM_REPOSITORY);
    assert_eq!(flm_row["build"]["tag"], FLM_TAG);
    assert_eq!(flm_row["build"]["asset"], FLM_ASSET);
    let package = install(&server, "fastflowlm", "npu").await;
    let payload = server
        .directory
        .path()
        .join("runtime-packages")
        .join(&package)
        .join("payload");
    assert!(payload.join("lib/libxrt_core.so.2").is_file());
    assert!(
        payload
            .join("lib/x86_64-linux-gnu/libxrt_core.so.2")
            .is_file()
    );

    let status = server.get("juiced/flm").await;
    assert_eq!(status["available"], true, "{status}");
    assert_eq!(status["package"], package);
    assert_eq!(status["validation"]["ready"], true);
    assert_eq!(status["models"][0]["checkpoint"], "gemma3:1b");
    assert_eq!(status["models"][0]["installed"], false);
    assert_eq!(status["models"][0]["kind"], "chat");
    assert_eq!(status["models"][1]["kind"], "embeddings");
    assert_eq!(status["store"], json!(server.directory.path().join("flm")));

    for (body, expected, code) in [
        (
            json!({"checkpoint":"gemma3:1b"}),
            StatusCode::CONFLICT,
            "flm_model_not_installed",
        ),
        (
            json!({"checkpoint":"../gemma"}),
            StatusCode::BAD_REQUEST,
            "invalid_checkpoint",
        ),
        (
            json!({"checkpoint":"embed-gemma:300m"}),
            StatusCode::CONFLICT,
            "flm_model_unsupported",
        ),
        (
            json!({"checkpoint":"qwen3:8b"}),
            StatusCode::NOT_FOUND,
            "flm_model_unknown",
        ),
    ] {
        let (status, error) = server.post("juiced/flm/models/add", body).await;
        assert_eq!(status, expected, "{error}");
        assert_eq!(error["error"]["code"], code);
    }

    let (code, accepted) = server
        .post(
            "juiced/flm/pull",
            json!({"checkpoint":"gemma3:1b","force":true}),
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{accepted}");
    let job_id = accepted["job"]["id"].as_str().unwrap().to_owned();
    let mut finished = Value::Null;
    for _ in 0..200 {
        let status = server.get("juiced/flm").await;
        let job = status["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == job_id)
            .cloned()
            .unwrap();
        if job["state"] != "pulling" {
            finished = job;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(finished["state"], "complete", "{finished}");
    assert_eq!(finished["percent"], 100);
    let store = server.directory.path().join("flm");
    // The job reports complete before the follow-up list/validate helper runs finish writing.
    let mut helpers = String::new();
    for _ in 0..200 {
        helpers = launches(&store.join("helpers.log"));
        if helpers.contains("argv:validate --json\n")
            && helpers.contains("argv:list --json --quiet\n")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        helpers.contains("argv:pull gemma3:1b --force\n"),
        "{helpers}"
    );
    assert!(helpers.contains("argv:list --json --quiet\n"), "{helpers}");
    assert!(helpers.contains("argv:validate --json\n"), "{helpers}");
    let payload_text = payload.display().to_string();
    assert!(
        helpers.contains(&format!(
            "LD_LIBRARY_PATH={payload_text}/lib/x86_64-linux-gnu:{payload_text}/lib\n"
        )),
        "{helpers}"
    );
    assert!(
        helpers.contains(&format!("XILINX_XRT={payload_text}/\n")),
        "{helpers}"
    );
    assert!(
        !helpers.contains("LEMONADE_") && !helpers.contains("missing-mirror"),
        "{helpers}"
    );

    let (code, added) = server
        .post(
            "juiced/flm/models/add",
            json!({"checkpoint":"gemma3:1b","context_tokens":2048,"policy":"on-demand"}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{added}");
    assert_eq!(added["model"]["id"], "gemma3-1b-FLM");
    assert_eq!(added["model"]["policy"], "on-demand");
    assert_eq!(added["engine"], "fastflowlm");
    assert_eq!(added["backend"], "npu");
    assert_eq!(added["package"], package);
    assert_eq!(added["resident_bytes"], 536870912);
    assert_eq!(added["execution_profile"]["adapter"], "fastflowlm");
    assert_eq!(
        added["execution_profile"]["args"],
        json!([
            "serve",
            "gemma3:1b",
            "--ctx-len",
            "2048",
            "--port",
            "{port}",
            "--host",
            "127.0.0.1",
            "--quiet"
        ])
    );
    let status = server.get("juiced/flm").await;
    assert_eq!(status["models"][0]["installed"], true);
    assert_eq!(status["models"][0]["registered"], json!(["gemma3-1b-FLM"]));

    let (code, loaded) = server.post("load", json!({"model":"gemma3-1b-FLM"})).await;
    assert_eq!(code, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["runtime_package"], package);
    let (code, reply) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":"gemma3-1b-FLM","messages":[{"role":"user","content":"npu"}]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{reply}");
    assert_eq!(
        reply["model"], "gemma3:1b",
        "FLM receives its checkpoint name"
    );
    let record = launches(&server.record);
    let serve = record
        .lines()
        .find(|line| line.starts_with("argv:serve "))
        .unwrap_or_else(|| panic!("{record}"));
    let port = serve.split_whitespace().nth(5).unwrap();
    assert!(port.parse::<u16>().is_ok_and(|port| port > 0), "{serve}");
    assert_eq!(
        serve,
        format!("argv:serve gemma3:1b --ctx-len 2048 --port {port} --host 127.0.0.1 --quiet")
    );
    assert!(
        record.contains(&format!("FLM_MODEL_PATH={}\n", store.display())),
        "{record}"
    );
    assert!(
        record.contains(&format!("XILINX_XRT={payload_text}/\n")),
        "{record}"
    );
    assert!(
        record.contains(&format!(
            "LD_LIBRARY_PATH={payload_text}/lib/x86_64-linux-gnu:{payload_text}/lib:"
        )),
        "{record}"
    );
    assert!(
        !record.contains("LEMONADE_") && !record.contains("missing-mirror"),
        "{record}"
    );
    let (code, _) = server
        .post("unload", json!({"model":"gemma3-1b-FLM"}))
        .await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn limited_memlock_keeps_npu_engines_unrecommended_with_a_reason() {
    let server = npu_server("65536\n").await;
    let info = server.get("system-info").await;
    assert_eq!(info["devices"]["limits"]["memlock"], "limited");
    assert_eq!(info["devices"]["limits"]["memlock_bytes"], 65536);
    let flm = variant(&info, "flm", "npu");
    assert_eq!(flm["state"], "unknown");
    assert_eq!(flm["reasons"][0]["code"], "memlock_limited");
    assert!(
        flm["reasons"][0]["message"]
            .as_str()
            .unwrap()
            .contains("--ulimit memlock=-1:-1")
    );
    assert_eq!(info["recommendations"]["flm"], Value::Null);
    let hybrid = variant(&info, "llamacpp", "hybrid-rocm");
    assert_eq!(hybrid["state"], "unknown");
    assert_eq!(
        engines(&server).await["recommendations"]["flm"],
        Value::Null
    );
}

#[tokio::test]
async fn hybrid_rocm_installs_registers_on_request_and_launches_with_native_environment() {
    let server = npu_server("unlimited\n").await;
    let document = engines(&server).await;
    assert_eq!(document["recommendations"]["llamacpp"], "rocm");
    let hybrid_row = row(&document, "llama.cpp", "hybrid-rocm");
    assert_eq!(hybrid_row["availability"], "available");
    assert_eq!(hybrid_row["recommended"], false);
    assert_eq!(hybrid_row["build"]["gpu_target"], "gfx1150");
    assert_eq!(hybrid_row["build"]["repository"], HYBRID_REPOSITORY);
    assert_eq!(hybrid_row["build"]["tag"], HYBRID_TAG);
    assert_eq!(hybrid_row["build"]["asset"], HYBRID_ASSET);
    let package = install(&server, "llama.cpp", "hybrid-rocm").await;
    let installed = engines(&server).await;
    assert_eq!(
        installed["installed"][0]["release"]["backend"],
        "hybrid-rocm"
    );
    assert_eq!(
        installed["installed"][0]["release"]["gpu_target"],
        "gfx1150"
    );

    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let (code, job) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":{
                "source_url":format!("{}/weights/fixture-model.gguf", server.github),
                "expected_sha256":digest(&server.weights),"expected_bytes":server.weights.len(),
                "maximum_bytes":server.weights.len()}}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{job}");
    let job_id = job["id"].as_str().unwrap().to_owned();
    let mut artifact = String::new();
    for _ in 0..200 {
        let job = server.get(&format!("juiced/artifacts/jobs/{job_id}")).await;
        if job["state"] == "complete" {
            artifact = job["artifact"].as_str().unwrap().to_owned();
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!artifact.is_empty());

    let (code, automatic) = server
        .post(
            "juiced/models/add",
            json!({"artifact":artifact,"id":"automatic"}),
        )
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{automatic}");
    assert_eq!(automatic["install"]["backend"], "rocm");
    let (code, added) = server
        .post(
            "juiced/models/add",
            json!({"artifact":artifact,"id":"hybrid","backend":"hybrid-rocm","policy":"on-demand"}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{added}");
    assert_eq!(added["backend"], "hybrid-rocm");
    assert_eq!(added["package"], package);
    assert!(added["reason"].as_str().unwrap().contains("was requested"));

    let (code, loaded) = server.post("load", json!({"model":"hybrid"})).await;
    assert_eq!(code, StatusCode::OK, "{loaded}");
    let (code, reply) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":"hybrid","messages":[{"role":"user","content":"hybrid"}]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{reply}");
    let record = launches(&server.record);
    let payload = server
        .directory
        .path()
        .join("runtime-packages")
        .join(&package)
        .join("payload")
        .display()
        .to_string();
    for expected in [
        "HYBRID_GPU_DEVICE=ROCm0".to_owned(),
        "HYBRID_REQUIRE_NPU=1".into(),
        "GGML_XDNA_GPU_DEVICE=ROCm0".into(),
        "GGML_XDNA_MAX_COPY_GB=2.000000".into(),
        "GGML_XDNA_MIN_BATCH=1024".into(),
        "GGML_XDNA_FAIL_CLOSED=1".into(),
        "GGML_XDNA_EXPLICIT_PHASE=1".into(),
        format!("GGML_BACKEND_PATH={payload}/libggml-xdna.so"),
        format!("XILINX_XRT={payload}/xrt"),
    ] {
        assert!(
            record.lines().any(|line| line == expected),
            "{expected}\n{record}"
        );
    }
    assert!(
        record.contains(&format!(
            "LD_LIBRARY_PATH={payload}/rocm/lib:{payload}/xrt/lib/x86_64-linux-gnu:{payload}/xrt/lib:{payload}/:"
        )),
        "{record}"
    );
    let argv = record
        .lines()
        .find(|line| line.starts_with("argv:"))
        .unwrap();
    assert!(
        argv.starts_with("argv:--device XDNA0,ROCm0 --tensor-split 0,1 --n-gpu-layers 99 --mmproj-device ROCm0 --batch-size 2048 --ubatch-size 2048 "),
        "{argv}"
    );
    assert!(argv.contains("--alias hybrid"), "{argv}");
    assert!(!argv.contains("--hybrid-"), "{argv}");
    assert!(!record.contains("LEMONADE_"), "{record}");
    let (code, _) = server.post("unload", json!({"model":"hybrid"})).await;
    assert_eq!(code, StatusCode::OK);

    let mut profile = added["execution_profile"].clone();
    profile["args"].as_array_mut().unwrap().extend(
        ["--hybrid-memory-mib", "3072", "--hybrid-min-prompt", "4096"].map(|value| json!(value)),
    );
    let registration = json!({
        "id": "hybrid-tuned",
        "artifact": artifact,
        "model_file": "model.gguf",
        "execution_profile": profile,
        "resident_bytes": added["model"]["resident_bytes"],
        "max_inflight": 1,
        "operations": ["chat/completions", "completions"],
    });
    let (code, registered) = server.post("models/register", registration).await;
    assert_eq!(code, StatusCode::OK, "{registered}");
    let (code, enabled) = server
        .post(
            "juiced/policy",
            json!({"model":"hybrid-tuned","policy":"on-demand","expected_revision":registered["revision"]}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{enabled}");
    std::fs::write(&server.record, "").unwrap();
    let (code, loaded) = server.post("load", json!({"model":"hybrid-tuned"})).await;
    assert_eq!(code, StatusCode::OK, "{loaded}");
    let record = launches(&server.record);
    assert!(
        record
            .lines()
            .any(|line| line == "GGML_XDNA_MAX_COPY_GB=3.000000"),
        "{record}"
    );
    assert!(
        record
            .lines()
            .any(|line| line == "GGML_XDNA_MIN_BATCH=4096"),
        "{record}"
    );
    assert!(!record.contains("--hybrid-"), "{record}");
    let (code, _) = server.post("unload", json!({"model":"hybrid-tuned"})).await;
    assert_eq!(code, StatusCode::OK);
}
