#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::Path, sync::Arc, time::Duration};
use support::{
    ADMIN_KEY, CLIENT_KEY, NATIVE_MANAGEMENT_ROUTES, Server, binary,
    huggingface::{Hub, HubFile},
    metadata_gguf,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const REPO: &str = "fixture/Tiny-GGUF";
const FILE: &str = "Tiny-Q4_0.gguf";
const ASSET: &str = "llama-b12032-bin-ubuntu-x64.tar.gz";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn llama_release() -> Vec<u8> {
    let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (path, mode, data) in [
        ("llama-b12032/llama-server", 0o755, worker.as_slice()),
        ("llama-b12032/LICENSE", 0o644, b"MIT fixture".as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, data).unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    encoder.finish().unwrap()
}

async fn fake_github(archive: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let archive = Arc::new(archive);
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
                let path = text.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = if path == "/api/repos/ggml-org/llama.cpp/releases/tags/b12032"
                {
                    (
                        "200 OK",
                        json!({"tag_name":"b12032","assets":[{"name":ASSET,"size":archive.len(),"digest":format!("sha256:{}", digest(&archive))}]})
                            .to_string()
                            .into_bytes(),
                    )
                } else if path == format!("/ggml-org/llama.cpp/releases/download/b12032/{ASSET}") {
                    ("200 OK", archive.to_vec())
                } else {
                    ("404 Not Found", Vec::new())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    (base, task)
}

fn cpu_only(root: &Path) {
    let cpuinfo: String = (0..4)
        .map(|processor| format!("processor\t: {processor}\nvendor_id\t: AuthenticAMD\nmodel name\t: Fixture CPU\nphysical id\t: 0\ncore id\t\t: {processor}\ncpu cores\t: 4\nflags\t\t: avx avx2 fma f16c\n\n"))
        .collect();
    std::fs::create_dir_all(root.join("proc")).unwrap();
    std::fs::write(root.join("proc/cpuinfo"), cpuinfo).unwrap();
    std::fs::write(
        root.join("proc/meminfo"),
        "MemTotal:       32763640 kB\nMemAvailable:   26446104 kB\n",
    )
    .unwrap();
}

struct Rig {
    server: Server,
    weights: Vec<u8>,
    _hub: Hub,
    _github: tokio::task::JoinHandle<()>,
    _hardware: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.stop();
        self._github.abort();
        fn writable(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
                for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        writable(&entry.path());
                    }
                }
            }
        }
        writable(&self.server.directory.path().join("runtime-packages"));
    }
}

async fn rig() -> Rig {
    let mut weights = metadata_gguf(2048);
    weights.resize(64 * 1024, 7);
    let hub = Hub::with_repos(BTreeMap::from([(
        REPO.to_owned(),
        vec![HubFile {
            path: FILE.into(),
            bytes: weights.clone(),
        }],
    )]))
    .await;
    let (github, task) = fake_github(llama_release()).await;
    let hardware = tempfile::tempdir().unwrap();
    cpu_only(hardware.path());
    let mut server = Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["resident_budget_bytes"] = json!(4u64 * 1024 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(10000);
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.set_environment("JUICED_TEST_ENGINE_RELEASES", &github);
    server.start().await;
    Rig {
        server,
        weights,
        _hub: hub,
        _github: task,
        _hardware: hardware,
    }
}

async fn admin(
    server: &Server,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    server.json(method, path, body, Some(ADMIN_KEY)).await
}

async fn events(server: &Server, path: &str, body: Value) -> Vec<(String, Value)> {
    let response = server
        .request(Method::POST, path, Some(body), Some(ADMIN_KEY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let text = response.text().await.unwrap();
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let kind = block
                .lines()
                .find_map(|line| line.strip_prefix("event: "))
                .unwrap_or("message")
                .to_owned();
            let data = block
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .map(|data| serde_json::from_str(data).unwrap())
                .unwrap_or(Value::Null);
            (kind, data)
        })
        .collect()
}

#[tokio::test]
async fn native_management_routes_require_operator_credentials() {
    let server = Server::new(true).await;
    for (method, path) in NATIVE_MANAGEMENT_ROUTES {
        let concrete = path.replace("{model}", "fixture.one");
        for key in [None, Some(CLIENT_KEY), Some("invalid-key")] {
            let response = server
                .request(
                    Method::from_bytes(method.as_bytes()).unwrap(),
                    &format!("/v1{concrete}"),
                    (method != "GET" && method != "DELETE").then(|| json!({})),
                    key,
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} {key:?}"
            );
        }
    }
    for (method, path) in [
        (Method::GET, "/internal/aliases"),
        (Method::POST, "/internal/aliases"),
        (Method::DELETE, "/internal/aliases/any"),
        (Method::POST, "/internal/pin"),
        (Method::POST, "/internal/shutdown"),
    ] {
        let response = server
            .request(method.clone(), path, None, Some(CLIENT_KEY))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }
    let (status, files) = server
        .json(
            Method::GET,
            "/v1/models/fixture.one/files",
            None,
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(files["files"][0]["role"], "main");
    assert!(files["files"][0].get("path").is_none());
    let (status, refused) = server
        .json(
            Method::GET,
            "/v1/models/fixture.one/files?include_paths=true",
            None,
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    let (status, files) = admin(
        &server,
        Method::GET,
        "/v1/models/fixture.one/files?include_paths=true",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(files["files"][0]["path"].is_string());
}

#[tokio::test]
async fn v092_install_pull_load_pin_alias_unload_delete_and_uninstall_flow() {
    let rig = rig().await;
    let server = &rig.server;

    let (status, dry) = admin(
        server,
        Method::POST,
        "/api/v1/install/dry-run",
        Some(json!({"recipe":"llamacpp","backend":"cpu"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dry}");
    assert_eq!(dry["filename"], ASSET);
    assert_eq!(dry["version"], "b12032");
    assert_eq!(dry["supported"], true);
    let (status, missing) = admin(
        server,
        Method::POST,
        "/api/v1/install/dry-run",
        Some(json!({"recipe":"llamacpp"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    let (status, installed) = admin(
        server,
        Method::POST,
        "/api/v1/install",
        Some(json!({"recipe":"llamacpp","backend":"cpu"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    assert_eq!(
        installed,
        json!({"status":"success","recipe":"llamacpp","backend":"cpu"})
    );
    let (_, info) = admin(server, Method::GET, "/api/v1/system-info", None).await;
    assert_eq!(
        info["recipes"]["llamacpp"]["backends"]["cpu"]["state"],
        "installed"
    );
    assert_eq!(
        info["recipes"]["llamacpp"]["backends"]["cpu"]["version"],
        "b12032"
    );

    let (status, variants) = admin(
        server,
        Method::GET,
        &format!("/v1/pull/variants?checkpoint={REPO}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{variants}");
    assert_eq!(variants["suggested_name"], "Tiny-GGUF");
    assert_eq!(variants["variants"][0]["name"], "Q4_0");
    assert_eq!(variants["variants"][0]["size_bytes"], rig.weights.len());
    let (status, found) = admin(
        server,
        Method::GET,
        "/v1/registry/search?query=tiny&format=gguf",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["format"], "gguf");
    assert_eq!(found["results"][0]["repository_id"], REPO);
    assert_eq!(found["results"][0]["has_gguf"], true);
    let (status, short) = admin(server, Method::GET, "/v1/registry/search?query=ab", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{short}");

    let refused = events(
        server,
        "/v1/pull",
        json!({"model_name":"Tiny","stream":true}),
    )
    .await;
    assert_eq!(refused[0].0, "error");
    assert_eq!(refused[0].1["code"], "unknown_model");
    let (status, registered) = admin(server, Method::POST, "/v1/models/register", Some(json!({"model_name":"user.Tiny","recipe":"llamacpp","checkpoint":format!("{REPO}:Q4_0")}))).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert_eq!(registered["model_name"], "Tiny");
    assert_eq!(registered["canonical_model_name"], "user.Tiny");
    assert_eq!(registered["model"]["downloaded"], false);
    let pulled = events(
        server,
        "/v1/pull",
        json!({"model_name":"user.Tiny","stream":true}),
    )
    .await;
    let (last, done) = pulled.last().unwrap();
    assert_eq!(last, "complete", "{pulled:?}");
    assert_eq!(done["total_files"], 1);
    assert!(pulled.iter().any(|(kind, data)| kind == "progress"
        && data["file"] == FILE
        && data["bytes_total"] == rig.weights.len()));
    let (_, downloads) = admin(server, Method::GET, "/v1/downloads", None).await;
    let job = &downloads.as_array().unwrap()[0];
    assert_eq!(job["id"], "model:user.Tiny");
    assert_eq!(job["status"], "completed");
    assert_eq!(job["complete"], true);
    assert_eq!(job["total_download_size"], rig.weights.len());
    let (status, again) = admin(
        server,
        Method::POST,
        "/v1/pull",
        Some(json!({"model_name":"Tiny"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        again,
        json!({"status":"success","message":"Installed model: Tiny"})
    );

    let (_, files) = admin(server, Method::GET, "/v1/models/user.Tiny/files", None).await;
    assert_eq!(files["model_id"], "user.Tiny");
    assert_eq!(
        files["files"],
        json!([{"name":FILE,"role":"main","size_bytes":rig.weights.len(),"exists":true}])
    );
    let (status, detail) = admin(server, Method::GET, "/v1/models/user.Tiny", None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["downloaded"], true);

    let (status, loaded) = admin(
        server,
        Method::POST,
        "/v1/load",
        Some(json!({"model_name":"user.Tiny","ctx_size":1024,"llamacpp_args":"-np 1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["status"], "success");
    assert_eq!(loaded["model"]["loaded"], true);
    assert_eq!(loaded["options_saved"], true);
    let options = server.get("models/Tiny/options").await;
    assert_eq!(options["saved"]["context_tokens"], 1024);
    assert_eq!(options["saved"]["parallel_slots"], 1);
    let (status, unsupported) = admin(
        server,
        Method::POST,
        "/v1/load",
        Some(json!({"model_name":"Tiny","llamacpp_args":"--flash-attn on"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{unsupported}");

    let (status, pinned) = admin(
        server,
        Method::POST,
        "/internal/pin",
        Some(json!({"model_name":"user.Tiny","pinned":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pinned}");
    assert_eq!(server.model("Tiny").await["policy"], "hot");
    admin(
        server,
        Method::POST,
        "/internal/pin",
        Some(json!({"model_name":"Tiny","pinned":false})),
    )
    .await;
    assert_eq!(server.model("Tiny").await["policy"], "on-demand");

    let (status, alias) = admin(
        server,
        Method::POST,
        "/internal/aliases",
        Some(json!({"alias":"tiny-alias","target":"user.Tiny"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{alias}");
    assert_eq!(
        alias,
        json!({"status":"ok","alias":"tiny-alias","target":"user.Tiny"})
    );
    let (_, aliases) = admin(server, Method::GET, "/internal/aliases", None).await;
    assert_eq!(
        aliases,
        json!({"aliases":[{"alias":"tiny-alias","target":"Tiny","downloaded":true,"recipe":"llamacpp"}]})
    );
    let (status, deleted) =
        admin(server, Method::DELETE, "/internal/aliases/tiny-alias", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted, json!({"status":"deleted","alias":"tiny-alias"}));
    let (status, _) = admin(server, Method::DELETE, "/internal/aliases/tiny-alias", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, unloaded) = admin(
        server,
        Method::POST,
        "/v1/unload",
        Some(json!({"model_name":"Tiny"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unloaded}");
    assert_eq!(
        unloaded,
        json!({"status":"success","message":"Model unloaded successfully"})
    );
    let (status, idle) = admin(
        server,
        Method::POST,
        "/v1/unload",
        Some(json!({"model_name":"Tiny"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{idle}");
    admin(
        server,
        Method::POST,
        "/v1/load",
        Some(json!({"model_name":"Tiny"})),
    )
    .await;
    let response = server
        .request(Method::POST, "/v1/unload", None, Some(ADMIN_KEY))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(server.model("Tiny").await["loaded"], false);

    let (status, reset) = admin(server, Method::DELETE, "/v1/models/Tiny/options", None).await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    assert_eq!(reset["saved"]["context_tokens"], Value::Null);

    let (status, busy) = admin(
        server,
        Method::POST,
        "/v1/uninstall",
        Some(json!({"recipe":"llamacpp","backend":"cpu"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{busy}");
    assert_eq!(busy["error"]["code"], "engine_in_use");

    let (status, removed) = admin(
        server,
        Method::POST,
        "/v1/delete",
        Some(json!({"model_name":"user.Tiny"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["message"], "Deleted model: user.Tiny");
    assert_eq!(removed["weights_removed"], true);
    assert_eq!(removed["freed_bytes"], rig.weights.len());
    let inventory = server.get("juiced/artifacts").await;
    assert!(
        inventory["artifacts"].as_array().unwrap().is_empty(),
        "{inventory}"
    );
    let (status, _) = admin(
        server,
        Method::POST,
        "/v1/delete",
        Some(json!({"model_name":"user.Tiny"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, uninstalled) = admin(
        server,
        Method::POST,
        "/v1/uninstall",
        Some(json!({"recipe":"llamacpp","backend":"cpu"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{uninstalled}");
    assert_eq!(uninstalled["removed_packages"].as_array().unwrap().len(), 1);
    let engines = server.get("juiced/engines").await;
    assert!(
        engines["installed"].as_array().unwrap().is_empty(),
        "{engines}"
    );
    let (status, again) = admin(
        server,
        Method::POST,
        "/v1/uninstall",
        Some(json!({"recipe":"llamacpp","backend":"cpu"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["removed_packages"], json!([]));

    let (status, control) = admin(
        server,
        Method::POST,
        "/v1/downloads/control",
        Some(json!({"id":"model:missing","action":"remove"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(control, json!({"status":"ok","missing":true}));
    let (status, _) = admin(
        server,
        Method::POST,
        "/v1/downloads/control",
        Some(json!({"id":"model:missing","action":"pause"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = admin(
        server,
        Method::POST,
        "/v1/downloads/control",
        Some(json!({"id":"model:user.Tiny","action":"remove"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        admin(server, Method::GET, "/v1/downloads", None).await.1,
        json!([])
    );
}

#[tokio::test]
async fn internal_shutdown_stops_the_service_gracefully() {
    let mut server = Server::new(true).await;
    let (status, body) = admin(&server, Method::POST, "/internal/shutdown", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "shutting_down");
    let exit = server.wait_for_manager_exit(Duration::from_secs(10)).await;
    assert!(exit.success(), "{exit:?}");
}
