mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::{CLIENT_KEY, PREFIXES, Server, binary};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

fn decode_base64(encoded: &str) -> Vec<u8> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("invalid base64"),
    };
    let mut output = Vec::new();
    for chunk in encoded.as_bytes().chunks(4) {
        let digits: Vec<u8> = chunk
            .iter()
            .filter(|byte| **byte != b'=')
            .map(|byte| value(*byte))
            .collect();
        let word = digits
            .iter()
            .enumerate()
            .fold(0u32, |word, (index, digit)| {
                word | (u32::from(*digit) << (18 - 6 * index))
            });
        for index in 0..digits.len().saturating_sub(1) {
            output.push((word >> (16 - 8 * index)) as u8);
        }
    }
    output
}

fn assert_png(encoded: &Value) {
    let bytes = decode_base64(encoded.as_str().expect("base64 image"));
    assert!(bytes.starts_with(PNG), "image is not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR");
}

fn wav_payload(bytes: &[u8]) -> Value {
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..16], b"WAVEfmt ");
    assert_eq!(&bytes[36..40], b"data");
    let length = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 44 + length);
    serde_json::from_slice(&bytes[44..]).unwrap()
}

async fn image(server: &Server, path: &str, body: Value, key: Option<&str>) -> (StatusCode, Value) {
    server.json(Method::POST, path, Some(body), key).await
}

async fn speech(server: &Server, path: &str, body: Value) -> (StatusCode, String, Vec<u8>) {
    let response = server
        .request(Method::POST, path, Some(body), Some(CLIENT_KEY))
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    (
        status,
        content_type,
        response.bytes().await.unwrap().to_vec(),
    )
}

async fn media_server(edit: impl FnOnce(&mut Value)) -> Server {
    Server::configured(true, |config| {
        let one = &mut config["models"][0];
        one["operations"] = json!(["images/generations"]);
        one["provider"] = json!("stable-diffusion-cpp");
        one["max_inflight"] = json!(1);
        let events = one["args"][5].clone();
        one["args"] = json!([
            "--listen-port",
            "{port}",
            "--model",
            "{model}",
            "--events",
            events
        ]);
        edit(config);
    })
    .await
}

#[tokio::test]
async fn image_generation_requires_a_selected_single_slot_worker_and_translates_requests() {
    let server = media_server(|_| {}).await;
    for prefix in PREFIXES {
        let path = format!("{prefix}/images/generations");
        let body = json!({"model":"fixture.one","prompt":"a lighthouse","size":"640x384","steps":3,"cfg_scale":1.5,"seed":11,"negative_prompt":"fog"});
        for key in [None, Some("invalid-key")] {
            let (status, _) = image(&server, &path, body.clone(), key).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
        let (status, reply) = image(&server, &path, body, Some(CLIENT_KEY)).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_png(&reply["data"][0]["b64_json"]);
        let received = &reply["received"];
        assert_eq!(received["size"], "640x384");
        assert_eq!(received["n"], 1);
        assert!(received.get("model").is_none());
        assert_eq!(reply["received_authorization"], false);
        let prompt = received["prompt"].as_str().unwrap();
        let extra = prompt
            .strip_prefix("a lighthouse <sd_cpp_extra_args>")
            .and_then(|rest| rest.strip_suffix("</sd_cpp_extra_args>"))
            .unwrap();
        let extra: Value = serde_json::from_str(extra).unwrap();
        assert_eq!(extra["sample_params"]["sample_steps"], 3);
        assert_eq!(extra["sample_params"]["guidance"]["txt_cfg"], 1.5);
        assert_eq!(extra["seed"], 11);
        assert_eq!(extra["negative_prompt"], "fog");
    }
    let (status, txt2img) = image(
        &server,
        "/sdapi/v1/txt2img",
        json!({"prompt":"a lighthouse","width":512,"height":512,"steps":2,"sampler_name":"euler_a"}),
        Some(CLIENT_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{txt2img}");
    assert_png(&txt2img["images"][0]);
    assert_eq!(txt2img["parameters"]["steps"], 2);
    assert_eq!(txt2img["parameters"]["sampler_name"], "euler_a");
    assert!(txt2img["parameters"].get("model").is_none());
    for key in [None, Some("invalid-key")] {
        let (status, _) = image(&server, "/sdapi/v1/txt2img", json!({"prompt":"x"}), key).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    for (path, body, status, code) in [
        (
            "/v1/images/generations",
            json!({"model":"fixture.one","prompt":"x","n":2}),
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
        (
            "/v1/images/generations",
            json!({"model":"fixture.one","prompt":"x","size":"4096x4096"}),
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/generations",
            json!({"model":"missing","prompt":"x"}),
            StatusCode::NOT_FOUND,
            "unknown_model",
        ),
        (
            "/v1/images/generations",
            json!({"model":"fixture.two","prompt":"x"}),
            StatusCode::NOT_IMPLEMENTED,
            "unsupported_operation",
        ),
        (
            "/v1/audio/speech",
            json!({"model":"fixture.one","input":"x","voice":"alloy"}),
            StatusCode::NOT_IMPLEMENTED,
            "unsupported_operation",
        ),
        (
            "/sdapi/v1/txt2img",
            json!({"prompt":"x","batch_size":5}),
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
    ] {
        let (actual, error) = image(&server, path, body, Some(CLIENT_KEY)).await;
        assert_eq!(actual, status, "{path}: {error}");
        assert_eq!(error["error"]["code"], code);
    }
    assert!(server.model("fixture.two").await["pid"].is_null());

    for (edit, message) in [
        (json!({"max_inflight":2}), "single slot"),
        (
            json!({"operations":["images/generations","chat/completions"]}),
            "only images",
        ),
    ] {
        let mut registration = support::canonical_model(json!({
            "id":"refused-image","model_file":server.config["models"][0]["model_file"],
            "provider":"stable-diffusion-cpp","command":binary("juiced-test-worker", "JUICED_TEST_WORKER"),
            "args":["--listen-port","{port}","--model","{model}"],"resident_bytes":1,"max_inflight":1,
            "operations":["images/generations"],
        }));
        for (key, value) in edit.as_object().unwrap() {
            registration[key] = value.clone();
        }
        let (status, error) = server.post("models/register", registration).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{message}: {error}");
    }
}

#[tokio::test]
async fn image_disconnect_retires_the_exact_worker_generation_and_recovers() {
    let server = media_server(|config| {
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--inference-delay-ms"), json!("1500")]);
    })
    .await;
    let (status, loaded) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let before = server.model("fixture.one").await;
    let socket = server
        .socket_request(
            "images/generations",
            json!({"model":"fixture.one","prompt":"held"}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "image-request", "started"))
        .await;
    assert_eq!(
        server.get("juiced/state").await["resources"]["active_requests"],
        1
    );
    drop(socket);
    server
        .wait_for(async || {
            let model = server.model("fixture.one").await;
            model["loaded"] == false
                && model["pid"].is_null()
                && server.get("juiced/state").await["resources"]["active_requests"] == 0
        })
        .await;
    assert!(!server.event("fixture.one", "image-request", "completed"));
    let records = server.get("juiced/requests").await;
    assert!(
        records["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["operation"] == "images/generations")
    );
    let (status, reply) = image(
        &server,
        "/v1/images/generations",
        json!({"model":"fixture.one","prompt":"after"}),
        Some(CLIENT_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_png(&reply["data"][0]["b64_json"]);
    let after = server.model("fixture.one").await;
    assert!(
        after["process_lifecycle"]["generation"].as_u64().unwrap()
            > before["process_lifecycle"]["generation"].as_u64().unwrap()
    );
    assert_eq!(after["policy"], before["policy"]);
    assert_eq!(after["revision"], before["revision"]);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod engines {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, io::Write, path::Path, sync::Arc};
    use support::huggingface::{Hub, HubFile};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    const SD_REPOSITORY: &str = "lemonade-sdk/stable-diffusion.cpp";
    const SD_TAG: &str = "master-957-90e86d1";
    const SD_ASSET: &str = "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64.zip";
    const KOKOROS_REPOSITORY: &str = "lemonade-sdk/Kokoros";
    const KOKOROS_TAG: &str = "b21";
    const KOKOROS_ASSET: &str = "kokoros-linux-x86_64.tar.gz";
    const SD_REPO: &str = "Green-Sky/SD-Turbo-GGUF";
    const SD_FILE: &str = "sd_turbo-f16-q8_0.gguf";
    const KOKORO_REPO: &str = "mikkoph/kokoro-onnx";
    const KOKORO_FILE: &str = "kokoro-v1.0.onnx";
    const VOICES_FILE: &str = "voices-v1.0.bin";

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn stored_zip(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
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

    fn tar_gz(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, mode, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);
            builder.append_data(&mut header, path, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    fn diffusion_gguf() -> Vec<u8> {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        let tensors = [
            "cond_stage_model.transformer.text_model.final_layer_norm.bias",
            "first_stage_model.decoder.conv_in.weight",
            "model.diffusion_model.input_blocks.0.0.weight",
        ];
        bytes.extend((tensors.len() as u64).to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
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

    fn kokoro_onnx() -> Vec<u8> {
        let mut bytes = b"\x08\x09\x12\x07pytorch\x1a\x052.6.0".to_vec();
        bytes.resize(8192, 0x5a);
        bytes
    }

    fn voices() -> Vec<u8> {
        stored_zip(&[
            ("af_heart.npy", 0o644, b"\x93NUMPY fixture heart"),
            ("am_adam.npy", 0o644, b"\x93NUMPY fixture adam"),
        ])
    }

    struct Releases {
        base: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Releases {
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

    async fn releases(
        assets: BTreeMap<(&'static str, &'static str, &'static str), Vec<u8>>,
    ) -> Releases {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let assets = Arc::new(assets);
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let assets = assets.clone();
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
                    let reply = if let Some(rest) = path.strip_prefix("/api/repos/") {
                        let listed: Vec<Value> = assets
                            .iter()
                            .filter(|((repository, tag, _), _)| {
                                rest == format!("{repository}/releases/tags/{tag}")
                            })
                            .map(|((_, _, asset), bytes)| {
                                json!({"name":asset,"size":bytes.len(),"digest":format!("sha256:{}", digest(bytes))})
                            })
                            .collect();
                        response(
                            "200 OK",
                            "Content-Type: application/json\r\n",
                            json!({"assets":listed}).to_string().as_bytes(),
                        )
                    } else if let Some(((_, _, _), bytes)) =
                        assets.iter().find(|((repository, tag, asset), _)| {
                            path == format!("/{repository}/releases/download/{tag}/{asset}")
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
        Releases { base, task }
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

    async fn settle_pull(server: &Server, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let view = server.get(&format!("juiced/library/pulls/{id}")).await;
            if matches!(view["state"].as_str(), Some("complete" | "failed")) {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "library pull did not settle: {view}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn install(server: &Server, engine: &str, backend: &str) -> String {
        let generation = server.get("juiced/engines").await["generation"].clone();
        let (status, accepted) = server
            .post(
                "juiced/engines/install",
                json!({"engine":engine,"backend":backend,"expected_generation":generation}),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        let id = accepted["job"]["id"].as_str().unwrap().to_owned();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let document = server.get("juiced/engines").await;
            let job = document["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == id)
                .cloned()
                .unwrap();
            if job["state"] == "complete" {
                assert_eq!(job["digest_source"], "github-release-api");
                return job["package"].as_str().unwrap().to_owned();
            }
            assert_ne!(job["state"], "failed", "{job}");
            assert!(Instant::now() < deadline, "engine install did not settle");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn catalog_image_and_speech_models_install_engines_add_and_generate_under_every_prefix() {
        let worker = std::fs::read(binary("juiced-test-worker", "JUICED_TEST_WORKER")).unwrap();
        let mut assets = BTreeMap::new();
        assets.insert(
            (SD_REPOSITORY, SD_TAG, SD_ASSET),
            stored_zip(&[
                ("sd-server", 0o755, &worker),
                ("libstable-diffusion.so", 0o755, b"fixture library bytes"),
                ("LICENSE", 0o644, b"MIT fixture"),
            ]),
        );
        assets.insert(
            (KOKOROS_REPOSITORY, KOKOROS_TAG, KOKOROS_ASSET),
            tar_gz(&[
                ("linux-x86_64/koko", 0o755, &worker),
                (
                    "linux-x86_64/espeak-ng-data/phontab",
                    0o644,
                    b"phoneme fixture",
                ),
            ]),
        );
        let github = releases(assets).await;
        let file = |path: &str, bytes: Vec<u8>| HubFile {
            path: path.to_owned(),
            bytes,
        };
        let hub = Hub::with_repos(BTreeMap::from([
            (SD_REPO.to_owned(), vec![file(SD_FILE, diffusion_gguf())]),
            (
                KOKORO_REPO.to_owned(),
                vec![
                    file(KOKORO_FILE, kokoro_onnx()),
                    file(VOICES_FILE, voices()),
                ],
            ),
        ]))
        .await;
        let hardware = tempfile::tempdir().unwrap();
        cpu_only(hardware.path());
        let mut server = Server::prepare(true, |config| {
            config["models"] = json!([]);
            config["artifact_source_policy"] = json!("allow-loopback-http");
            config["resident_budget_bytes"] = json!(8u64 * 1024 * 1024 * 1024);
            config["startup_timeout_ms"] = json!(10000);
        });
        server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
        server.set_environment("JUICED_TEST_ENGINE_RELEASES", &github.base);
        server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
        server.start().await;

        let document = server.get("juiced/engines").await;
        assert_eq!(document["recommendations"]["sd-cpp"], "cpu");
        assert_eq!(document["recommendations"]["kokoro"], "cpu");
        let rows = document["installable"].as_array().unwrap();
        let row = |engine: &str, backend: &str| {
            rows.iter()
                .find(|row| row["engine"] == engine && row["backend"] == backend)
                .unwrap_or_else(|| panic!("missing {engine}:{backend}"))
                .clone()
        };
        assert_eq!(row("sd.cpp", "cpu")["build"]["asset"], SD_ASSET);
        assert_eq!(row("sd.cpp", "cuda")["unresolved"], "no_matching_gpu");
        assert_eq!(row("kokoros", "cpu")["build"]["asset"], KOKOROS_ASSET);

        let search = server.get("juiced/library/search?q=kokoro&task=tts").await;
        let kokoro_entry = search["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["catalog_id"] == "kokoro-v1")
            .cloned()
            .unwrap();
        let bundle = &kokoro_entry["quants"][0];
        assert_eq!(bundle["file"], KOKORO_FILE, "{kokoro_entry}");
        assert_eq!(bundle["files"].as_array().unwrap().len(), 2);
        assert_eq!(bundle["recommended"], true);
        assert_eq!(
            bundle["sha256"],
            hub.file(KOKORO_REPO, KOKORO_FILE).sha256()
        );

        let mut artifacts = BTreeMap::new();
        for (repo, path) in [(SD_REPO, SD_FILE), (KOKORO_REPO, KOKORO_FILE)] {
            let generation = server.get("juiced/artifacts").await["generation"].clone();
            let (status, started) = server
                .post(
                    "juiced/library/pull",
                    json!({"repo":repo,"file":path,"expected_generation":generation}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{started}");
            let done = settle_pull(&server, started["pull"].as_str().unwrap()).await;
            assert_eq!(done["state"], "complete", "{done}");
            artifacts.insert(repo, hub.file(repo, path).sha256());
            if repo == KOKORO_REPO {
                assert_eq!(done["set_state"], "published", "{done}");
                assert_eq!(done["suggested_id"], "kokoro-v1", "{done}");
                let set = server
                    .get(&format!(
                        "juiced/artifacts/sets/{}",
                        started["set"].as_str().unwrap()
                    ))
                    .await;
                let roles: Vec<&str> = set["specification"]["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|member| member["role"].as_str().unwrap())
                    .collect();
                assert_eq!(roles, ["model", "voice"]);
            }
        }

        let (status, refused) = server
            .post("juiced/models/add", json!({"artifact":artifacts[SD_REPO]}))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["error"]["code"], "engine_not_installed");
        assert_eq!(refused["install"]["engine"], "sd.cpp");
        assert_eq!(refused["install"]["backend"], "cpu");
        let sd_package = install(&server, "sd.cpp", "cpu").await;
        let kokoro_package = install(&server, "kokoros", "cpu").await;

        let (status, image_model) = server
            .post(
                "juiced/models/add",
                json!({"artifact":artifacts[SD_REPO],"policy":"on-demand"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{image_model}");
        assert_eq!(image_model["engine"], "sd.cpp");
        assert_eq!(image_model["package"], sd_package);
        assert_eq!(image_model["format"]["format"], "diffusion");
        assert_eq!(image_model["format"]["includes_vae"], true);
        assert_eq!(
            image_model["model"]["operations"],
            json!(["images/generations"])
        );
        let image_id = image_model["model"]["id"].as_str().unwrap().to_owned();
        assert_eq!(image_id, "sd_turbo-f16-q8_0");

        let (status, speech_model) = server
            .post(
                "juiced/models/add",
                json!({"artifact":artifacts[KOKORO_REPO],"policy":"on-demand"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{speech_model}");
        assert_eq!(speech_model["engine"], "kokoros");
        assert_eq!(speech_model["package"], kokoro_package);
        assert_eq!(speech_model["format"]["format"], "onnx");
        assert_eq!(
            speech_model["companions"],
            json!([{"name":VOICES_FILE,"role":"voice"}])
        );
        assert!(speech_model["artifact_set"].is_string());
        let speech_id = speech_model["model"]["id"].as_str().unwrap().to_owned();
        assert_eq!(speech_id, "kokoro-v1");

        for prefix in PREFIXES {
            let (status, reply) = image(
                &server,
                &format!("{prefix}/images/generations"),
                json!({"model":image_id,"prompt":"a cabin in snow","size":"512x512","steps":4}),
                Some(CLIENT_KEY),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{reply}");
            assert_png(&reply["data"][0]["b64_json"]);
            assert_eq!(reply["received"]["size"], "512x512");

            let (status, content_type, bytes) = speech(
                &server,
                &format!("{prefix}/audio/speech"),
                json!({"model":speech_id,"input":"Hello from Juiced.","voice":"af_heart","response_format":"wav","speed":1.1}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(content_type, "audio/wav");
            let received = wav_payload(&bytes);
            assert_eq!(received["voice"], "af_heart");
            assert_eq!(received["input"], "Hello from Juiced.");
            assert_eq!(received["stream"], false);
            assert_eq!(received["model"], "kokoro");
            assert_eq!(received["received_authorization"], false);
            assert!(
                received["espeak_data"].as_str().is_some_and(|path| path
                    .ends_with("espeak-ng-data/phontab")
                    && path.len() < 160)
            );

            let response = server
                .request(
                    Method::POST,
                    &format!("{prefix}/audio/speech"),
                    Some(json!({"model":speech_id,"input":"x","voice":"alloy"})),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

            let (status, voices) = server
                .json(
                    Method::GET,
                    &format!("{prefix}/juiced/voices?model={speech_id}"),
                    None,
                    Some(CLIENT_KEY),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{voices}");
            assert_eq!(voices["data"][0]["model"], speech_id);
            assert_eq!(voices["data"][0]["voices"], json!(["af_heart", "am_adam"]));
            assert_eq!(voices["aliases"]["alloy"], "af_alloy");
        }
        let (status, mp3, bytes) = speech(
            &server,
            "/v1/audio/speech",
            json!({"model":speech_id,"input":"Short.","voice":"alloy"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(mp3, "audio/mpeg");
        assert!(bytes.starts_with(b"ID3"));
        let (status, error) = server
            .json(
                Method::GET,
                &format!("/v1/juiced/voices?model={image_id}"),
                None,
                Some(CLIENT_KEY),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "voices_unavailable");
        let listed = server.get("juiced/voices").await;
        assert_eq!(listed["data"].as_array().unwrap().len(), 1);

        let (status, txt2img) = image(
            &server,
            "/sdapi/v1/txt2img",
            json!({"prompt":"a cabin","width":512,"height":512}),
            Some(CLIENT_KEY),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{txt2img}");
        assert_png(&txt2img["images"][0]);

        let wyoming =
            support::voice::wyoming(&server, json!({"enabled": true, "listen": "127.0.0.1:0"}))
                .await;
        assert_eq!(wyoming["resolved"]["tts_model"], speech_id);
        let mut client =
            support::voice::Wyoming::connect(wyoming["listener"]["address"].as_str().unwrap())
                .await;
        client.send("describe", json!({}), &[]).await;
        let info = client.next(Duration::from_secs(5)).await.unwrap();
        let voices: Vec<_> = info.data["tts"][0]["voices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|voice| voice["name"].as_str().unwrap())
            .collect();
        assert_eq!(voices, ["af_heart", "am_adam"]);
        let (start, pcm, chunks) = client
            .synthesize(json!({"text": "Hello from Wyoming.", "voice": {"name": "am_adam"}}))
            .await;
        assert_eq!(start.kind, "audio-start", "{:?}", start.data);
        assert_eq!(start.data["rate"], 24_000);
        assert!(chunks >= 1 && !pcm.is_empty() && pcm.len() % 2 == 0);
        let summary = server.get("juiced/wyoming").await["last_event"].clone();
        assert_eq!(summary["outcome"], "completed");
        assert_eq!(summary["model"], speech_id);
        support::voice::wyoming(&server, json!({"enabled": false})).await;

        let catalog = server.get("juiced/catalog").await;
        let speech_view = catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == speech_id)
            .unwrap()
            .clone();
        assert_eq!(speech_view["implementation_id"], "kokoros");
        assert_eq!(speech_view["runtime_package"], kokoro_package);
        let (status, _) = server.post("unload", json!({"model":speech_id})).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = server.post("unload", json!({"model":image_id})).await;
        assert_eq!(status, StatusCode::OK);
    }
}
