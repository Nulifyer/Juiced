#![cfg(unix)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
use support::{
    Server, binary, cli,
    filesystem::{Entry, digest, tree},
    metadata_gguf, rejected_manager,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Stores {
    server: Server,
    _aliases: tempfile::TempDir,
    foreign: tempfile::TempDir,
    original: PathBuf,
    alias: PathBuf,
    original_config: PathBuf,
    foreign_before: BTreeMap<PathBuf, Entry>,
}
impl Stores {
    fn prepare() -> Self {
        let aliases = tempfile::tempdir().unwrap();
        let mut server = Server::prepare(true, |config| {
            let root = Path::new(config["models"][0]["model_file"].as_str().unwrap())
                .parent()
                .unwrap()
                .to_owned();
            let bundle = root.join("runtime bundle");
            std::fs::create_dir(&bundle).unwrap();
            let name = format!("juiced-test-worker{}", std::env::consts::EXE_SUFFIX);
            let executable = bundle.join(&name);
            std::fs::copy(
                binary("juiced-test-worker", "JUICED_TEST_WORKER"),
                &executable,
            )
            .unwrap();
            let sha256 = digest(&executable);
            std::fs::create_dir(root.join("models")).unwrap();
            std::fs::write(
                root.join("models").join("weights with space.gguf"),
                metadata_gguf(8192),
            )
            .unwrap();
            config["cache_enabled"] = json!(true);
            config["artifact_source_policy"] = json!("allow-loopback-http");
            for model in config["models"].as_array_mut().unwrap() {
                model["model_file"] = json!("models/weights with space.gguf");
                model["command"] = json!(Path::new("runtime bundle").join(&name));
                model["provider"] = json!("llama-cpp");
                model["runtime_bundle"] = json!({"schema_version":1,"implementation_id":"llama-cpp","root":"runtime bundle","entrypoint":name,"library_directories":[],"files":[{"path":name,"sha256":sha256,"role":"executable"}]});
                model["max_inflight"] = json!(1);
                model["options"] =
                    json!({"parallel_slots":1,"context_tokens":512,"threads":2,"gpu_layers":0});
                model["args"]
                    .as_array_mut()
                    .unwrap()
                    .extend([json!("--slot-save-path"), json!("{cache}")]);
            }
        });
        let original = std::fs::canonicalize(server.directory.path()).unwrap();
        let original_config = original.join("config.json");
        let foreign = tempfile::tempdir().unwrap();
        std::fs::create_dir(foreign.path().join("runtime bundle")).unwrap();
        let name = format!("juiced-test-worker{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(
            original.join("runtime bundle").join(&name),
            foreign.path().join("runtime bundle").join(&name),
        )
        .unwrap();
        std::fs::create_dir(foreign.path().join("models")).unwrap();
        std::fs::write(
            foreign
                .path()
                .join("models")
                .join("weights with space.gguf"),
            metadata_gguf(1024),
        )
        .unwrap();
        let mut config = server.config.clone();
        config["generation"] = json!(9);
        config["models"][1]["policy"] = json!("disabled");
        config["models"][1]["revision"] = json!(9);
        std::fs::write(
            foreign.path().join("config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
        for name in ["cache", "artifacts"] {
            std::fs::create_dir(foreign.path().join(name)).unwrap();
            std::fs::write(
                foreign.path().join(name).join("foreign-marker"),
                b"foreign store must remain unchanged",
            )
            .unwrap();
        }
        let alias = aliases.path().join("active store");
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        server.set_working_directory(aliases.path());
        server.config_path = Path::new("active store").join("config.json");
        let foreign_before = tree(foreign.path());
        Self {
            server,
            _aliases: aliases,
            foreign,
            original,
            alias,
            original_config,
            foreign_before,
        }
    }
    fn retarget(&self) {
        std::fs::remove_file(&self.alias).unwrap();
        std::os::unix::fs::symlink(self.foreign.path(), &self.alias).unwrap();
    }
    fn foreign_unchanged(&self) {
        assert_eq!(tree(self.foreign.path()), self.foreign_before);
    }
}
#[cfg(target_os = "linux")]
fn actual_model_path(stores: &Stores, pid: &Value) {
    use std::os::unix::ffi::OsStrExt;
    let pid = pid.as_u64().unwrap();
    let file = std::fs::File::open(format!("/proc/{pid}/cmdline")).unwrap();
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 2 * 1024 * 1024);
    let arguments = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let model = arguments
        .windows(2)
        .find(|pair| pair[0] == b"--model")
        .unwrap()[1];
    assert_eq!(
        Path::new(std::ffi::OsStr::from_bytes(model)),
        stores
            .original
            .join("models")
            .join("weights with space.gguf")
    );
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
        stores.original.join("runtime bundle").join(format!(
            "juiced-test-worker{}",
            std::env::consts::EXE_SUFFIX
        ))
    );
}
async fn infer(server: &Server, model: &str, session: &str) -> Value {
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", session)
        .json(&json!({"model":model,"prompt":"Canonical config-root pipeline"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["model"], model);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    body
}

#[tokio::test]
async fn live_config_parent_alias_retarget_cannot_redirect_relative_models_bundles_or_cli_policy_writes()
 {
    let mut stores = Stores::prepare();
    let original_bytes = std::fs::read(&stores.original_config).unwrap();
    stores.server.start().await;
    assert_eq!(
        std::fs::read(&stores.original_config).unwrap(),
        original_bytes
    );
    let first = cli::call(&stores.server, &["load", "fixture.one"], None).await;
    stores.retarget();
    infer(&stores.server, "fixture.one", "alias-live").await;
    let retained = stores.server.model("fixture.one").await;
    assert_eq!(retained["pid"], first["pid"]);
    assert_eq!(
        retained["process_lifecycle"]["generation"],
        first["process_lifecycle"]["generation"]
    );
    assert_eq!(
        stores.server.get("juiced/context?model=fixture.one").await["declared"]["training_context_tokens"],
        8192
    );
    #[cfg(target_os = "linux")]
    actual_model_path(&stores, &retained["pid"]);
    let second = cli::call(&stores.server, &["load", "fixture.two"], None).await;
    infer(&stores.server, "fixture.two", "alias-cold").await;
    assert_eq!(
        stores.server.get("juiced/context?model=fixture.two").await["declared"]["training_context_tokens"],
        8192
    );
    #[cfg(target_os = "linux")]
    actual_model_path(&stores, &second["pid"]);
    cli::call(&stores.server, &["unload", "fixture.two"], None).await;
    let revision = second["revision"].as_u64().unwrap().to_string();
    let saved = cli::call(
        &stores.server,
        &["policy", "fixture.two", "disabled", "--revision", &revision],
        None,
    )
    .await;
    let disk: Value =
        serde_json::from_slice(&std::fs::read(&stores.original_config).unwrap()).unwrap();
    assert_eq!(disk["schema_version"], 2);
    assert_eq!(disk["models"][1]["policy"], "disabled");
    assert_eq!(disk["models"][1]["revision"], saved["revision"]);
    assert_eq!(
        disk["models"][1]["model_file"],
        "models/weights with space.gguf"
    );
    let still_live = stores.server.model("fixture.one").await;
    assert_eq!(still_live["pid"], retained["pid"]);
    assert_eq!(
        still_live["process_lifecycle"]["generation"],
        retained["process_lifecycle"]["generation"]
    );
    stores.foreign_unchanged();
    stores.server.stop();
    stores.server.config_path = stores.original_config.clone();
    stores.server.start().await;
    assert_eq!(
        stores.server.model("fixture.two").await["policy"],
        "disabled"
    );
    assert_eq!(
        stores.server.model("fixture.two").await["revision"],
        saved["revision"]
    );
    cli::call(&stores.server, &["load", "fixture.one"], None).await;
    infer(&stores.server, "fixture.one", "alias-restart").await;
    stores.foreign_unchanged();
}

struct Source {
    url: String,
    body: Vec<u8>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Source {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Source {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/artifact", listener.local_addr().unwrap());
        let body = b"canonical config root artifact fixture".to_vec();
        let payload = body.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"config-root-fixture\"\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                if socket.write_all(header.as_bytes()).await.is_ok()
                    && !request.starts_with(b"HEAD ")
                {
                    let _ = socket.write_all(&payload).await;
                }
            }
        });
        Self { url, body, task }
    }
}

#[tokio::test]
async fn retargeted_config_alias_cannot_redirect_default_cache_or_artifact_pipeline_effects() {
    let mut stores = Stores::prepare();
    let original_bytes = std::fs::read(&stores.original_config).unwrap();
    stores.server.start().await;
    stores.retarget();
    infer(&stores.server, "fixture.one", "alias-cache").await;
    assert!(stores.original.join("cache").is_dir());
    assert!(stores.original.join("artifacts").is_dir());
    #[cfg(target_os = "linux")]
    let snapshot = {
        let before = stores.server.get("juiced/cache").await;
        let(status,saved)=stores.server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"alias-cache","expected_generation":before["generation"],"maximum_bytes":1024})).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
        assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
        let path = stores.original.join("cache").join(format!(
            "kv-{}.bin",
            saved["snapshot"]["id"].as_str().unwrap()
        ));
        assert!(std::fs::metadata(path).unwrap().len() > 0);
        saved["snapshot"]["id"].clone()
    };
    let source = Source::new().await;
    let sha256 = format!("{:x}", Sha256::digest(&source.body));
    let before = stores.server.get("juiced/artifacts").await;
    let(status,job)=stores.server.post("juiced/artifacts/pull",json!({"expected_generation":before["generation"],"download":{"source_url":source.url,"expected_sha256":sha256,"expected_bytes":source.body.len(),"maximum_bytes":source.body.len()}})).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let job_path = format!("juiced/artifacts/jobs/{}", job["id"].as_str().unwrap());
    stores
        .server
        .wait_for(async || stores.server.get(&job_path).await["state"] == "complete")
        .await;
    let complete = stores.server.get(&job_path).await;
    let path = stores.original.join("artifacts").join(format!(
        "artifact-{}.bin",
        complete["artifact"].as_str().unwrap()
    ));
    assert_eq!(std::fs::read(path).unwrap(), source.body);
    assert_eq!(
        std::fs::read(&stores.original_config).unwrap(),
        original_bytes
    );
    stores.foreign_unchanged();
    #[cfg(target_os = "linux")]
    {
        stores.server.stop();
        stores.server.config_path = stores.original_config.clone();
        stores.server.start().await;
        cli::call(&stores.server, &["load", "fixture.one"], None).await;
        let (status, restored) = stores
            .server
            .post(
                "juiced/cache/restore",
                json!({"model":"fixture.one","slot":0,"session":"alias-cache","snapshot":snapshot}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
        assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
        assert_eq!(
            infer(&stores.server, "fixture.one", "alias-cache").await["fixture_restores"],
            1
        );
        assert_eq!(
            stores.server.get(&job_path).await["artifact"],
            complete["artifact"]
        );
        assert_eq!(
            std::fs::read(&stores.original_config).unwrap(),
            original_bytes
        );
        stores.foreign_unchanged();
    }
}

#[tokio::test]
async fn config_leaf_symlinks_reject_before_listener_even_with_an_allowed_parent_alias() {
    for alias_parent in [false, true] {
        let aliases = tempfile::tempdir().unwrap();
        let server = Server::prepare(false, |_| {});
        let original = server.config_path.clone();
        let target = server.directory.path().join("config-target.json");
        std::fs::rename(&original, &target).unwrap();
        std::os::unix::fs::symlink(&target, &original).unwrap();
        let input = if alias_parent {
            let alias = aliases.path().join("store");
            std::os::unix::fs::symlink(server.directory.path(), &alias).unwrap();
            alias.join("config.json")
        } else {
            original.clone()
        };
        let before = std::fs::read(&target).unwrap();
        let output = rejected_manager(&input).await;
        assert!(!output.stderr.is_empty());
        assert!(
            std::fs::symlink_metadata(&original)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), before);
        assert!(!server.event("fixture.one", "worker-startup", "started"));
    }
}
