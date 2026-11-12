#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use support::{ADMIN_KEY, CLIENT_KEY, PREFIXES, Server, binary, metadata_gguf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const LLAMA_REPOSITORY: &str = "ggml-org/llama.cpp";
const LLAMA_TAG: &str = "b11393";
const LLAMA_ASSET: &str = "llama-b11393-bin-ubuntu-x64.tar.gz";
const REVISION: &str = "50968a4468ef4233ed78cd7c3de230dd1d61a56b";
const MARKER: &str = "JUICED_SYNTHETIC_ONLINE_IMPORT_MARKER";

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

fn release_archive(server: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, mode, data) in [
        ("llama-b11393/llama-server", 0o755, server),
        ("llama-b11393/LICENSE", 0o644, b"MIT fixture".as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, data).unwrap();
    }
    let tar = builder.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).unwrap();
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
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let reply = if path
                    == format!("/api/repos/{LLAMA_REPOSITORY}/releases/tags/{LLAMA_TAG}")
                {
                    let body = json!({"tag_name":LLAMA_TAG,"assets":[{"name":LLAMA_ASSET,"size":archive.len(),"digest":format!("sha256:{}", digest(&archive))}]});
                    response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        body.to_string().as_bytes(),
                    )
                } else if path
                    == format!("/{LLAMA_REPOSITORY}/releases/download/{LLAMA_TAG}/{LLAMA_ASSET}")
                {
                    response("302 Found", "Location: /objects/llama\r\n", b"")
                } else if path == "/objects/llama" {
                    response("200 OK", "", &archive)
                } else {
                    response("404 Not Found", "", b"")
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    (base, task)
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

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    files
}

struct Volume {
    root: tempfile::TempDir,
}

impl Volume {
    fn new() -> Self {
        let volume = Self {
            root: tempfile::tempdir().unwrap(),
        };
        let config = volume.config();
        write(
            &config.join("recipe_options.json"),
            format!(
                r#"{{"builtin.Qwen3-0.6B-GGUF":{{"ctx_size":2048,"llamacpp_args":"-t 2 --flash-attn {MARKER}","llamacpp_backend":"cuda","pinned":true,"{MARKER}":1}},"builtin.Stale-Model":{{}}}}"#
            ),
        );
        write(
            &config.join("execution_profiles.json"),
            r#"{"builtin.Qwen3-0.6B-GGUF":{"selected":"llamacpp:cuda","profiles":{"llamacpp:cuda":{"id":"llamacpp:cuda","recipe":"llamacpp","options":{}}}}}"#,
        );
        write(
            &config.join("user_models.json"),
            r#"{"MyModel":{"checkpoint":"acme/My-GGUF:Q8_0","recipe":"llamacpp"},"Missing":{"checkpoint":"acme/Gone-GGUF","recipe":"llamacpp"}}"#,
        );
        write(
            &config.join("config.json"),
            format!(r#"{{"max_loaded_models":2,"note":"{MARKER}"}}"#),
        );
        write(
            &config.join("juiced/state.json"),
            r#"{"version":1,"policies":{"user.MyModel":"on-demand"}}"#,
        );
        write(
            &volume.path().join("cache/aliases.json"),
            r#"{"chat":"Qwen3-0.6B-GGUF","mine":"user.MyModel","dangling":"Nope"}"#,
        );
        let hub = volume.path().join("huggingface/hub");
        let qwen = hub.join("models--unsloth--Qwen3-0.6B-GGUF");
        write(
            &qwen
                .join("snapshots")
                .join(REVISION)
                .join("Qwen3-0.6B-Q4_0.gguf"),
            metadata_gguf(40960),
        );
        write(&qwen.join("refs/main"), REVISION);
        write(
            &qwen.join(".lemonade_registry.json"),
            format!(
                r#"{{"processed_models":{{"Qwen3-0.6B-GGUF":{{"snapshot_id":"{REVISION}"}}}}}}"#
            ),
        );
        write(
            &hub.join("models--acme--My-GGUF/snapshots/abc123/My-Q8_0.gguf"),
            metadata_gguf(4096),
        );
        volume
    }
    fn path(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path()).unwrap()
    }
    fn config(&self) -> PathBuf {
        self.root.path().join("config")
    }
}

async fn server(volume: &Volume, archive_server: &str) -> (Server, tempfile::TempDir) {
    let hardware = tempfile::tempdir().unwrap();
    cpu_only(hardware.path());
    let roots = json!([volume.path()]);
    let mut server = Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["resident_budget_bytes"] = json!(4u64 * 1024 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(10000);
        config["import_roots"] = roots;
    });
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.set_environment("JUICED_TEST_ENGINE_RELEASES", archive_server);
    server.start().await;
    (server, hardware)
}

fn model<'a>(document: &'a Value, native_id: &str) -> &'a Value {
    document["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["native_id"] == native_id)
        .unwrap_or_else(|| panic!("missing {native_id}: {document}"))
}

async fn install_cpu_engine(server: &Server) -> String {
    let (status, accepted) = server
        .post(
            "juiced/engines/install",
            json!({"engine":"llama.cpp","backend":"cpu","expected_generation":0}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let id = accepted["job"]["id"].as_str().unwrap().to_owned();
    for _ in 0..600 {
        let document = server.get("juiced/engines").await;
        let job = document["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id.as_str())
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

#[tokio::test]
async fn online_native_import_previews_applies_in_place_and_reapplies_idempotently() {
    let volume = Volume::new();
    let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
    let (base, github) = fake_github(release_archive(&worker)).await;
    let (server, _hardware) = server(&volume, &base).await;
    let source = volume.config();
    let before = tree(volume.root.path());
    let preview_body = json!({"action":"preview","source":source});

    for prefix in PREFIXES {
        let path = format!("{prefix}/juiced/import/native");
        for key in [None, Some(CLIENT_KEY)] {
            let response = server
                .request(Method::POST, &path, Some(preview_body.clone()), key)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        let (status, preview) = server
            .json(
                Method::POST,
                &path,
                Some(preview_body.clone()),
                Some(ADMIN_KEY),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["configuration_generation"], 0);
    }
    let (status, preview) = server
        .post("juiced/import/native", preview_body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let text = preview.to_string();
    assert!(!text.contains(MARKER), "{text}");
    assert_eq!(
        preview["weights_root"],
        json!(volume.path().join("huggingface/hub"))
    );
    let qwen = model(&preview, "builtin.Qwen3-0.6B-GGUF");
    assert_eq!(qwen["proposed_id"], "Qwen3-0.6B-GGUF");
    assert_eq!(qwen["status"], "engine_install_needed");
    assert_eq!(qwen["importable"], true);
    assert_eq!(qwen["weights"]["found"], true);
    assert_eq!(qwen["weights"]["snapshot"], REVISION);
    assert_eq!(
        qwen["weights"]["path"],
        json!(volume.path().join(format!(
            "huggingface/hub/models--unsloth--Qwen3-0.6B-GGUF/snapshots/{REVISION}/Qwen3-0.6B-Q4_0.gguf"
        )))
    );
    assert_eq!(qwen["format"]["format"], "gguf");
    assert_eq!(qwen["options"]["context_tokens"], 2048);
    assert_eq!(qwen["options"]["threads"], 2);
    assert_eq!(qwen["engine"]["engine"], "llama.cpp");
    assert_eq!(qwen["engine"]["backend"], "cpu");
    assert_eq!(qwen["engine"]["native_backend"], "cuda");
    assert_eq!(qwen["engine"]["engine_install_needed"], true);
    assert_eq!(
        qwen["engine"]["install"],
        json!({"engine":"llama.cpp","backend":"cpu","gpu_target":null,"installable":true,"expected_generation":0})
    );
    assert_eq!(qwen["previous_policy"], "hot");
    assert_eq!(qwen["previous_policy_source"], "pinned");
    assert_eq!(qwen["suggested_policy"], "hot");
    assert_eq!(qwen["default_policy"], "disabled");
    assert_eq!(qwen["selected_profile"], "llamacpp:cuda");
    let unsupported = qwen["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == "llamacpp_args" && row["state"] == "unsupported")
        .unwrap();
    assert_eq!(unsupported["value"], json!(["--flash-attn"]));
    let mine = model(&preview, "user.MyModel");
    assert_eq!(mine["status"], "engine_install_needed");
    assert_eq!(mine["previous_policy"], "on-demand");
    assert_eq!(model(&preview, "user.Missing")["status"], "blocked");
    assert_eq!(
        model(&preview, "user.Missing")["blockers"][0]["code"],
        "weights_missing"
    );
    assert_eq!(
        model(&preview, "builtin.Stale-Model")["blockers"][0]["code"],
        "native_row_unrecognized"
    );
    let aliases = preview["aliases"].as_array().unwrap();
    assert_eq!(aliases.len(), 3);
    assert!(
        aliases
            .iter()
            .any(|alias| alias["alias"] == "chat" && alias["state"] == "importable")
    );
    assert!(
        aliases
            .iter()
            .any(|alias| alias["alias"] == "dangling" && alias["state"] == "target_not_imported")
    );
    let global = preview["global_settings"].as_array().unwrap();
    assert!(
        global
            .iter()
            .any(|row| row["key"] == "max_loaded_models" && row["state"] == "unsupported")
    );
    assert!(global.iter().any(|row| row["key"] == "note"
        && row["state"] == "unknown"
        && row.get("value").is_none()));

    let outside = tempfile::tempdir().unwrap();
    let (status, error) = server
        .post(
            "juiced/import/native",
            json!({"action":"preview","source":outside.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    assert_eq!(error["error"]["code"], "import_path_outside_roots");
    let (status, error) = server
        .post(
            "juiced/import/native",
            json!({"action":"preview","source":source,"extra":1}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"]["code"], "invalid_request");

    let apply = |generation: u64| {
        json!({"action":"apply","source":source,"aliases":true,"expected_generation":generation,"models":[
            {"native_id":"builtin.Qwen3-0.6B-GGUF","policy":"on-demand"},
            {"native_id":"user.MyModel","id":"my-model"},
            {"native_id":"user.Missing"},
            {"native_id":"builtin.Stale-Model","include":false},
        ]})
    };
    let (status, pending) = server.post("juiced/import/native", apply(0)).await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending["configuration_generation"], 0);
    assert_eq!(
        model(&pending, "builtin.Qwen3-0.6B-GGUF")["status"],
        "engine_install_needed"
    );
    assert_eq!(model(&pending, "user.Missing")["status"], "blocked");
    assert_eq!(model(&pending, "builtin.Stale-Model")["status"], "skipped");
    assert!(
        pending["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|alias| alias["status"] != "added")
    );
    assert!(
        server.get("juiced/catalog").await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let package = install_cpu_engine(&server).await;
    let (status, preview) = server
        .post("juiced/import/native", preview_body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let qwen = model(&preview, "builtin.Qwen3-0.6B-GGUF");
    assert_eq!(qwen["status"], "ready", "{qwen}");
    assert_eq!(qwen["engine"]["package"], package);
    assert_eq!(qwen["execution_profile"]["options"]["context_tokens"], 2048);
    assert_eq!(qwen["execution_profile"]["options"]["threads"], 2);
    assert_eq!(qwen["execution_profile"]["options"]["gpu_layers"], 0);

    let (status, conflict) = server
        .post(
            "juiced/import/native",
            json!({"action":"apply","source":source,"expected_generation":7,"models":[{"native_id":"user.MyModel"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "revision_conflict");
    let (status, unknown) = server
        .post(
            "juiced/import/native",
            json!({"action":"apply","source":source,"expected_generation":0,"models":[{"native_id":"user.Nobody"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");

    let (status, applied) = server.post("juiced/import/native", apply(0)).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["source_modified"], false);
    assert_eq!(applied["weights_copied"], false);
    assert_eq!(applied["configuration_generation"], 2, "{applied}");
    let qwen = model(&applied, "builtin.Qwen3-0.6B-GGUF");
    assert_eq!(qwen["status"], "imported");
    assert_eq!(qwen["policy"], "on-demand");
    let mine = model(&applied, "user.MyModel");
    assert_eq!(
        (&mine["status"], &mine["id"], &mine["policy"]),
        (&json!("imported"), &json!("my-model"), &json!("disabled"))
    );
    let added: BTreeMap<_, _> = applied["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alias| {
            (
                alias["alias"].as_str().unwrap().to_owned(),
                alias["status"].clone(),
            )
        })
        .collect();
    assert_eq!(added["chat"], "added");
    assert_eq!(added["mine"], "added");
    assert_eq!(added["dangling"], "skipped_target_not_imported");

    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    let models = saved["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    let spec = models
        .iter()
        .find(|model| model["id"] == "Qwen3-0.6B-GGUF")
        .unwrap();
    assert_eq!(spec["model_file"], qwen_path(&volume));
    assert!(spec.get("artifact").is_none());
    assert_eq!(spec["execution_profile"]["runtime_package"], package);
    assert_eq!(
        saved["aliases"]["entries"],
        json!({"chat":"Qwen3-0.6B-GGUF","mine":"my-model"})
    );
    assert_eq!(
        server.get("juiced/artifacts").await["artifacts"]
            .as_array()
            .map_or(0, Vec::len),
        0
    );

    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":"chat","messages":[{"role":"user","content":"imported"}]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, disabled) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":"my-model","messages":[{"role":"user","content":"x"}]})),
            Some(CLIENT_KEY),
        )
        .await;
    assert!(status.is_client_error(), "{disabled}");

    let (status, again) = server.post("juiced/import/native", apply(2)).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["configuration_generation"], 2, "{again}");
    assert_eq!(
        model(&again, "builtin.Qwen3-0.6B-GGUF")["status"],
        "already_imported"
    );
    assert_eq!(
        model(&again, "builtin.Qwen3-0.6B-GGUF")["policy"],
        "on-demand"
    );
    assert_eq!(model(&again, "user.MyModel")["status"], "already_imported");
    assert!(
        again["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|alias| alias["status"] != "added")
    );
    let (status, preview) = server.post("juiced/import/native", preview_body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        model(&preview, "builtin.Qwen3-0.6B-GGUF")["status"],
        "already_imported"
    );
    assert_eq!(
        model(&preview, "builtin.Qwen3-0.6B-GGUF")["already_imported"],
        "Qwen3-0.6B-GGUF"
    );

    let (status, clash) = server
        .post(
            "juiced/import/native",
            json!({"action":"apply","source":source,"expected_generation":2,"models":[{"native_id":"user.MyModel","id":"Qwen3-0.6B-GGUF"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{clash}");
    assert_eq!(clash["error"]["code"], "model_exists");
    let (status, clash) = server
        .post(
            "juiced/import/native",
            json!({"action":"apply","source":source,"expected_generation":2,"models":[{"native_id":"user.MyModel","id":"chat"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{clash}");
    assert_eq!(clash["error"]["code"], "alias_conflict");
    assert_eq!(tree(volume.root.path()), before);
    github.abort();
}

fn qwen_path(volume: &Volume) -> Value {
    json!(volume.path().join(format!(
        "huggingface/hub/models--unsloth--Qwen3-0.6B-GGUF/snapshots/{REVISION}/Qwen3-0.6B-Q4_0.gguf"
    )))
}

#[tokio::test]
async fn online_native_import_refuses_damaged_documents_and_unconfigured_roots() {
    let volume = Volume::new();
    write(
        &volume.config().join("execution_profiles.json"),
        format!(
            r#"{{"builtin.Qwen3-0.6B-GGUF":{{"selected":"{MARKER}","profiles":{{}},"settings":{{"version":9,"saved_options":{{}}}}}}}}"#
        ),
    );
    let before = tree(volume.root.path());
    let (server, _hardware) = server(&volume, "http://127.0.0.1:9").await;
    let (status, error) = server
        .post(
            "juiced/import/native",
            json!({"action":"preview","source":volume.config()}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "model_settings_recovery_required");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("execution_profiles.json")
    );
    assert!(!error.to_string().contains(MARKER));
    let (status, error) = server
        .post(
            "juiced/import/native",
            json!({"action":"apply","source":volume.config(),"expected_generation":0,"models":[]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(tree(volume.root.path()), before);
    let logs = server.get("juiced/logs").await.to_string();
    assert!(!logs.contains(MARKER));

    let mut unconfigured = Server::prepare(true, |config| config["models"] = json!([]));
    unconfigured.start().await;
    let (status, error) = unconfigured
        .post(
            "juiced/import/native",
            json!({"action":"preview","source":volume.config()}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    assert_eq!(error["error"]["code"], "import_roots_unconfigured");
}
