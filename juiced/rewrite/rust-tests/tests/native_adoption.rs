#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use support::{Server, binary, metadata_gguf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const LLAMA: (&str, &str, &str) = (
    "ggml-org/llama.cpp",
    "b12032",
    "llama-b12032-bin-ubuntu-x64.tar.gz",
);
const SD: (&str, &str, &str) = (
    "lemonade-sdk/stable-diffusion.cpp",
    "master-957-90e86d1",
    "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64.zip",
);
const KOKOROS: (&str, &str, &str) = ("lemonade-sdk/Kokoros", "b21", "kokoros-linux-x86_64.tar.gz");
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OLDER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

fn tar_gz(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, mode, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(*mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, *data).unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    encoder.finish().unwrap()
}

fn zip(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut central = Vec::new();
    for (name, mode, data) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let offset = output.len() as u32;
        let mut common = Vec::new();
        common.extend(20u16.to_le_bytes());
        common.extend(0u16.to_le_bytes());
        common.extend(0u16.to_le_bytes());
        common.extend(0u16.to_le_bytes());
        common.extend(0x21u16.to_le_bytes());
        common.extend(crc.sum().to_le_bytes());
        common.extend((data.len() as u32).to_le_bytes());
        common.extend((data.len() as u32).to_le_bytes());
        common.extend((name.len() as u16).to_le_bytes());
        common.extend(0u16.to_le_bytes());
        output.extend(0x0403_4b50u32.to_le_bytes());
        output.extend(&common);
        output.extend(name.as_bytes());
        output.extend(*data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(((3u16 << 8) | 20).to_le_bytes());
        central.extend(&common);
        central.extend(0u16.to_le_bytes());
        central.extend(0u16.to_le_bytes());
        central.extend(0u16.to_le_bytes());
        central.extend(((0o100000 | mode) << 16).to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let start = output.len() as u32;
    output.extend(&central);
    output.extend(0x0605_4b50u32.to_le_bytes());
    output.extend(0u16.to_le_bytes());
    output.extend(0u16.to_le_bytes());
    output.extend((entries.len() as u16).to_le_bytes());
    output.extend((entries.len() as u16).to_le_bytes());
    output.extend((central.len() as u32).to_le_bytes());
    output.extend(start.to_le_bytes());
    output.extend(0u16.to_le_bytes());
    output
}

fn gguf(architecture: &str, tensors: &[&str]) -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend((tensors.len() as u64).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    let key = "general.architecture";
    bytes.extend((key.len() as u64).to_le_bytes());
    bytes.extend(key.as_bytes());
    bytes.extend(8u32.to_le_bytes());
    bytes.extend((architecture.len() as u64).to_le_bytes());
    bytes.extend(architecture.as_bytes());
    for name in tensors {
        bytes.extend((name.len() as u64).to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(16u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
    }
    bytes.resize(bytes.len() + 4096, 0);
    bytes
}

fn safetensors(name: &str) -> Vec<u8> {
    let header = json!({name: {"dtype":"F16","shape":[1],"data_offsets":[0,2]}}).to_string();
    let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
    bytes.extend(header.as_bytes());
    bytes.extend([0, 0]);
    bytes
}

fn voices() -> Vec<u8> {
    zip(&[
        ("af_heart.npy", 0o644, b"\x93NUMPY heart"),
        ("am_adam.npy", 0o644, b"\x93NUMPY adam"),
    ])
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

struct Releases {
    base: String,
    downloads: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Releases {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn releases(worker: &[u8]) -> Releases {
    let mut assets: BTreeMap<(&str, &str, &str), Vec<u8>> = BTreeMap::new();
    assets.insert(
        LLAMA,
        tar_gz(&[
            ("llama-b12032/llama-server", 0o755, worker),
            ("llama-b12032/LICENSE", 0o644, b"MIT"),
        ]),
    );
    assets.insert(
        SD,
        zip(&[("sd-server", 0o755, worker), ("LICENSE", 0o644, b"MIT")]),
    );
    assets.insert(
        KOKOROS,
        tar_gz(&[
            ("linux-x86_64/koko", 0o755, worker),
            ("linux-x86_64/espeak-ng-data/phontab", 0o644, b"phonemes"),
        ]),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let assets = Arc::new(assets);
    let downloads = Arc::new(Mutex::new(Vec::new()));
    let seen = downloads.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let assets = assets.clone();
            let seen = seen.clone();
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
                let reply = if let Some(rest) = path.strip_prefix("/api/repos/") {
                    let listed: Vec<Value> = assets
                        .iter()
                        .filter(|((repository, tag, _), _)| {
                            rest == format!("{repository}/releases/tags/{tag}")
                        })
                        .map(|((_, _, asset), bytes)| {
                            json!({"name":asset,"size":bytes.len(),"digest":format!("sha256:{:x}", Sha256::digest(bytes))})
                        })
                        .collect();
                    response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        json!({"assets": listed}).to_string().as_bytes(),
                    )
                } else if let Some(((_, _, asset), bytes)) =
                    assets.iter().find(|((repository, tag, asset), _)| {
                        path == format!("/{repository}/releases/download/{tag}/{asset}")
                    })
                {
                    seen.lock().unwrap().push((*asset).to_owned());
                    response("200 OK", "", bytes)
                } else {
                    response("404 Not Found", "", b"")
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    Releases {
        base,
        downloads,
        task,
    }
}

fn cpu_only(root: &Path) {
    let mut cpuinfo = String::new();
    for processor in 0..8 {
        cpuinfo.push_str(&format!(
            "processor\t: {processor}\nvendor_id\t: AuthenticAMD\nmodel name\t: Fixture CPU\nphysical id\t: 0\ncore id\t\t: {}\ncpu cores\t: 4\nflags\t\t: avx avx2 fma f16c\n\n",
            processor % 4
        ));
    }
    write(&root.join("proc/cpuinfo"), cpuinfo);
    write(
        &root.join("proc/meminfo"),
        "MemTotal:       32763640 kB\nMemAvailable:   26446104 kB\n",
    );
}

/// A native v0.9.2 volume: `/data` holds `config/` and the `HF_HOME` hub, `/library` holds a
/// read-only lemonade hub and local speech models.
struct Volume {
    root: tempfile::TempDir,
}

impl Volume {
    fn data(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path())
            .unwrap()
            .join("data")
    }
    fn library(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path())
            .unwrap()
            .join("library")
    }
    fn config(&self) -> PathBuf {
        self.data().join("config")
    }
    fn snapshot(&self, hub: &Path, repo: &str, revision: &str) -> PathBuf {
        hub.join(format!("models--{}", repo.replace('/', "--")))
            .join("snapshots")
            .join(revision)
    }
    fn reference(&self, hub: &Path, repo: &str, revision: &str) {
        write(
            &hub.join(format!("models--{}", repo.replace('/', "--")))
                .join("refs/main"),
            revision,
        );
    }

    fn new() -> Self {
        let volume = Self {
            root: tempfile::tempdir().unwrap(),
        };
        let config = volume.config();
        let listener = volume.library().join("asr/models/listener-Q8_0.gguf");
        write(
            &config.join("user_models.json"),
            json!({
                "Chat-Q6_K": {"checkpoints": {"main": "acme/Chat-GGUF:Q6_K", "mmproj": "acme/Chat-GGUF:mmproj-BF16.gguf"},
                    "labels": ["chat", "custom", "vision"], "recipe": "llamacpp", "source": "huggingface"},
                "Writer-IQ4_NL": {"checkpoints": {"main": "acme/Writer-GGUF:IQ4_NL"}, "recipe": "llamacpp",
                    "recipe_options": {"ctx_size": 4096, "llamacpp_args": "--temp 1.0 --top-k 20",
                        "llamacpp_backend": "rocm", "merge_args": true}},
                "Picture-Q8_0": {"checkpoints": {"main": "acme/Picture-GGUF:picture-Q8_0.gguf",
                        "text_encoder": "acme/Encoder-GGUF:encoder-Q4_K_M.gguf",
                        "vae": "acme/Vae:split_files/vae/ae.safetensors"},
                    "image_defaults": {"cfg_scale": 1.0, "height": 768, "steps": 8, "width": 768},
                    "recipe": "sd-cpp",
                    "recipe_options": {"cfg_scale": 1.0, "height": 768, "sampling_method": "euler",
                        "sd-cpp_backend": "vulkan", "sdcpp_args": "--scheduler simple -t 8", "steps": 8, "width": 768}},
                "Listener-Q8_0": {"checkpoints": {"main": listener}, "recipe": "asr4all", "source": "local_path",
                    "recipe_options": {"asr4all_commit": "256", "asr4all_right": "32", "asr4all_threads": "4"}},
                "Retired-Hybrid": {"checkpoints": {"main": "acme/Writer-GGUF:Writer-IQ4_NL.gguf"}, "recipe": "hybrid",
                    "juiced_existing_artifact": true,
                    "recipe_options": {"hybrid_copy_gib": 16, "llamacpp_backend": "system", "ctx_size": 2048}},
                "Gone": {"checkpoint": "acme/Gone-GGUF:Q4_0", "recipe": "llamacpp"},
            })
            .to_string(),
        );
        write(
            &config.join("recipe_options.json"),
            json!({
                "user.Chat-Q6_K": {"llamacpp_args": "--temp 1.0 --chat-template-kwargs '{\"preserve_thinking\":true}' --spec-type draft-mtp --parallel 1 -ngl 999 --host 0.0.0.0 --lora /private/adapter.gguf",
                    "llamacpp_backend": "rocm", "pinned": true},
                "user.Writer-IQ4_NL": {"pinned": false},
                "user.Listener-Q8_0": {"pinned": false},
                "builtin.kokoro-v1": {},
            })
            .to_string(),
        );
        write(
            &config.join("execution_profiles.json"),
            r#"{"user.Chat-Q6_K":{"selected":"registered","profiles":{"registered":{"id":"registered","recipe":"llamacpp","options":{"llamacpp_backend":"rocm"}}}}}"#,
        );
        write(
            &config.join("config.json"),
            r#"{"config_version":2,"host":"0.0.0.0","port":8080,"max_loaded_models":4,"llamacpp":{"vulkan_bin":"/opt/juiced/llama"}}"#,
        );
        write(
            &config.join("juiced/state.json"),
            r#"{"version":1,"policies":{"Writer-IQ4_NL":"on-demand","kokoro-v1":"hot","Picture-Q8_0":"disabled","Retired-Hybrid":"disabled"},"snapshots":{}}"#,
        );
        write(
            &volume.data().join("cache/aliases.json"),
            r#"{"assistant":"user.Chat-Q6_K"}"#,
        );
        let hub = volume.data().join("huggingface/hub");
        let library = volume.library().join("huggingface/hub");
        let chat = volume.snapshot(&hub, "acme/Chat-GGUF", REVISION);
        for shard in [
            "Q6_K/Chat-Q6_K-00001-of-00002.gguf",
            "Q6_K/Chat-Q6_K-00002-of-00002.gguf",
        ] {
            write(&chat.join(shard), metadata_gguf(65536));
        }
        write(&chat.join("mmproj-BF16.gguf"), gguf("clip", &[]));
        volume.reference(&hub, "acme/Chat-GGUF", REVISION);
        let writer = volume.snapshot(&library, "acme/Writer-GGUF", OLDER);
        write(&writer.join("Writer-IQ4_NL.gguf"), metadata_gguf(32768));
        write(
            &volume
                .snapshot(&library, "acme/Writer-GGUF", REVISION)
                .join("Writer-Q8_0.gguf"),
            metadata_gguf(32768),
        );
        volume.reference(&library, "acme/Writer-GGUF", REVISION);
        write(
            &volume
                .snapshot(&hub, "acme/Picture-GGUF", REVISION)
                .join("picture-Q8_0.gguf"),
            gguf(
                "z_image",
                &["model.diffusion_model.input_blocks.0.0.weight"],
            ),
        );
        write(
            &volume
                .snapshot(&library, "acme/Encoder-GGUF", REVISION)
                .join("encoder-Q4_K_M.gguf"),
            metadata_gguf(4096),
        );
        write(
            &volume
                .snapshot(&hub, "acme/Vae", REVISION)
                .join("split_files/vae/ae.safetensors"),
            safetensors("first_stage_model.decoder.conv_in.weight"),
        );
        write(&listener, gguf("asr4all", &[]));
        let kokoro = volume.snapshot(&hub, "mikkoph/kokoro-onnx", REVISION);
        write(
            &kokoro.join("index.json"),
            r#"{"model":"kokoro-v1.0.onnx","voices":"voices-v1.0.bin"}"#,
        );
        let mut onnx = b"\x08\x09\x12\x07pytorch\x1a\x052.6.0".to_vec();
        onnx.resize(8192, 0x5a);
        write(&kokoro.join("kokoro-v1.0.onnx"), onnx);
        write(&kokoro.join("voices-v1.0.bin"), voices());
        volume.reference(&hub, "mikkoph/kokoro-onnx", REVISION);
        std::fs::create_dir_all(volume.data().join("import")).unwrap();
        volume
    }
}

async fn adopted(server: &Server) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let state = server.get("juiced/state").await["native_adoption"].clone();
        if matches!(state["state"].as_str(), Some("adopted" | "failed")) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "adoption did not settle: {state}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn outcome<'a>(state: &'a Value, id: &str) -> &'a Value {
    state["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}: {state}"))
}

fn saved_model(server: &Server, id: &str) -> Value {
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    saved["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} not registered: {saved}"))
}

fn arguments(model: &Value) -> Vec<String> {
    model["execution_profile"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|argument| argument.as_str().unwrap().to_owned())
        .collect()
}

fn has_pair(arguments: &[String], flag: &str, value: &str) -> bool {
    arguments
        .windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

#[tokio::test]
async fn first_boot_adopts_a_native_volume_in_place_and_restarts_idempotently() {
    let volume = Volume::new();
    let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
    let github = releases(&worker).await;
    let hardware = tempfile::tempdir().unwrap();
    cpu_only(hardware.path());
    let import = volume.data().join("import");
    let mut server = Server::prepare(false, |config| {
        config["models"] = json!([]);
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["resident_budget_bytes"] = json!(8u64 * 1024 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(10000);
        config["import_roots"] = json!([import]);
    });
    let launches = server.directory.path().join("launches.log");
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.set_environment("JUICED_TEST_ENGINE_RELEASES", &github.base);
    server.set_environment("JUICED_NATIVE_CONFIG_DIR", volume.config());
    server.set_environment(
        "JUICED_NATIVE_IMPORT_ROOTS",
        format!("{}:{}", volume.data().display(), volume.library().display()),
    );
    server.set_environment("FIXTURE_LAUNCH_RECORD", &launches);
    let native = tree(&volume.config());
    server.start().await;

    let state = adopted(&server).await;
    assert_eq!(state["source_modified"], false, "{state}");
    assert_eq!(state["weights_copied"], false);
    for (id, expected, policy) in [
        ("Chat-Q6_K", "registered", Some("hot")),
        ("Writer-IQ4_NL", "registered", Some("on-demand")),
        ("kokoro-v1", "registered", Some("hot")),
        ("Retired-Hybrid", "registered", Some("disabled")),
        ("Picture-Q8_0", "waiting_for_engine", None),
        ("Listener-Q8_0", "waiting_for_engine", None),
        ("Gone", "blocked", None),
    ] {
        let row = outcome(&state, id);
        assert_eq!(row["outcome"], expected, "{row}");
        assert_eq!(row["policy"], json!(policy), "{row}");
    }
    assert_eq!(
        outcome(&state, "Gone")["blockers"][0]["code"],
        "weights_missing"
    );
    let pending = state["marker"]["pending"].clone();
    assert_eq!(pending, json!(["user.Listener-Q8_0", "user.Picture-Q8_0"]));
    let downloaded: BTreeSet<String> = github.downloads.lock().unwrap().iter().cloned().collect();
    assert_eq!(
        downloaded,
        BTreeSet::from([LLAMA.2.to_owned(), KOKOROS.2.to_owned()]),
        "Disabled models must not install engines"
    );

    let models = server.get("models").await;
    let ids: BTreeSet<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        BTreeSet::from(["Chat-Q6_K", "Writer-IQ4_NL", "kokoro-v1", "assistant"])
    );

    let chat = saved_model(&server, "Chat-Q6_K");
    let chat_path = volume
        .snapshot(
            &volume.data().join("huggingface/hub"),
            "acme/Chat-GGUF",
            REVISION,
        )
        .join("Q6_K/Chat-Q6_K-00001-of-00002.gguf");
    assert_eq!(chat["model_file"], json!(chat_path));
    assert!(chat.get("artifact").is_none());
    assert_eq!(chat["companions"][0]["name"], "mmproj");
    assert_eq!(chat["companions"][0]["role"], "projector");
    let args = arguments(&chat);
    assert!(has_pair(&args, "--mmproj", "{member:mmproj}"), "{args:?}");
    assert!(has_pair(
        &args,
        "--chat-template-kwargs",
        r#"{"preserve_thinking":true}"#
    ));
    assert!(has_pair(&args, "--spec-type", "draft-mtp"));
    assert!(has_pair(&args, "--temp", "1.0"));
    assert!(
        !args
            .iter()
            .any(|argument| argument == "--lora" || argument == "0.0.0.0")
    );
    assert_eq!(
        chat["execution_profile"]["options"]["context_tokens"],
        65536
    );
    let writer = saved_model(&server, "Writer-IQ4_NL");
    assert!(writer["model_file"].as_str().unwrap().contains(&format!(
        "library/huggingface/hub/models--acme--Writer-GGUF/snapshots/{OLDER}"
    )));
    assert_eq!(
        writer["execution_profile"]["options"]["context_tokens"],
        4096
    );
    let hybrid = saved_model(&server, "Retired-Hybrid");
    assert_eq!(
        hybrid["execution_profile"]["options"]["context_tokens"],
        2048
    );
    let kokoro = saved_model(&server, "kokoro-v1");
    assert_eq!(kokoro["companions"][0]["role"], "voice");
    let voices = server.get("juiced/voices").await;
    let listed = voices["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model"] == "kokoro-v1")
        .unwrap();
    assert_eq!(listed["voices"], json!(["af_heart", "am_adam"]), "{voices}");

    let deadline = Instant::now() + Duration::from_secs(20);
    let record = loop {
        let record = std::fs::read_to_string(&launches).unwrap_or_default();
        if record.contains("--mmproj") {
            break record;
        }
        if Instant::now() >= deadline {
            let catalog = server.get("juiced/catalog").await;
            let logs = server.get("juiced/logs?limit=100").await;
            panic!("the hot model did not launch: {record} {catalog} {logs}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let projector = chat_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mmproj-BF16.gguf");
    assert!(
        record.contains(&format!("--mmproj {}", projector.display())),
        "{record}"
    );
    assert!(record.contains("--alias Chat-Q6_K"));
    let (status, reply) = server
        .post(
            "chat/completions",
            json!({"model":"assistant","messages":[{"role":"user","content":"hi"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    let (status, accepted) = server
        .post(
            "juiced/engines/install",
            json!({"engine":"sd.cpp","backend":"cpu","expected_generation":server.get("juiced/engines").await["generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let catalog = server.get("juiced/catalog").await;
        if catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "Picture-Q8_0")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the pending model did not register: {catalog}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let state = adopted(&server).await;
    assert_eq!(
        state["marker"]["pending"],
        json!(["user.Listener-Q8_0"]),
        "{state}"
    );
    let picture = saved_model(&server, "Picture-Q8_0");
    assert_eq!(picture["policy"], "disabled");
    let args = arguments(&picture);
    assert!(has_pair(&args, "--diffusion-model", "{model}"), "{args:?}");
    assert!(has_pair(&args, "--llm", "{member:text_encoder}"));
    assert!(has_pair(&args, "--vae", "{member:vae}"));
    for (flag, value) in [
        ("--scheduler", "simple"),
        ("-t", "8"),
        ("--steps", "8"),
        ("-W", "768"),
        ("-H", "768"),
        ("--sampling-method", "euler"),
        ("--cfg-scale", "1"),
    ] {
        assert!(has_pair(&args, flag, value), "{flag} {args:?}");
    }
    let roles: BTreeSet<&str> = picture["companions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|companion| companion["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, BTreeSet::from(["encoder", "vae"]));

    let generation = saved_generation(&server);
    server.restart().await;
    let state = adopted(&server).await;
    assert_eq!(state["marker"]["pending"], json!(["user.Listener-Q8_0"]));
    assert_eq!(
        saved_generation(&server),
        generation,
        "restart repeated adoption"
    );
    let catalog = server.get("juiced/catalog").await;
    let policies: BTreeMap<&str, &str> = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| {
            (
                model["id"].as_str().unwrap(),
                model["policy"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        policies,
        BTreeMap::from([
            ("Chat-Q6_K", "hot"),
            ("Picture-Q8_0", "disabled"),
            ("Retired-Hybrid", "disabled"),
            ("Writer-IQ4_NL", "on-demand"),
            ("kokoro-v1", "hot"),
        ])
    );

    std::fs::remove_file(&projector).unwrap();
    server.restart().await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let catalog = server.get("juiced/catalog").await;
        let chat = catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == "Chat-Q6_K")
            .cloned()
            .unwrap();
        if chat["error"] == "companion_missing" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a missing companion was not reported: {chat}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(tree(&volume.config()), native, "native files changed");
}

fn saved_generation(server: &Server) -> u64 {
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    saved["generation"].as_u64().unwrap()
}
