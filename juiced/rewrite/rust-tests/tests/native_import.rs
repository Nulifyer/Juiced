#![cfg(unix)]

mod support;

use juiced_service::native_store::descriptor::STORE_ROOT;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use support::{
    PREFIXES, Server,
    artifacts::Source,
    binary,
    filesystem::{Entry, digest, tree},
    metadata_gguf,
};
use tokio::io::AsyncReadExt;

const MODEL: &str = "user.Tiny";
const SOURCE_MARKER: &str = "PRIVATE_NATIVE_SOURCE_OPAQUE_4832c40a";

#[cfg(target_os = "linux")]
use support::llama::flat_cpu_bundle;

struct Inputs {
    server: Server,
    _source: tempfile::TempDir,
    source: PathBuf,
    bindings: PathBuf,
    binding: Value,
    artifact: String,
}
impl Inputs {
    async fn prepare() -> Self {
        Self::prepare_from(None, metadata_gguf(8192)).await
    }
    async fn prepare_from(real_engine: Option<&Path>, weights: Vec<u8>) -> Self {
        let mut runtime = None;
        let server=Server::configured(true,|config| {
            config["cache_enabled"]=json!(true);
            config["artifact_source_policy"]=json!("allow-loopback-http");
            if real_engine.is_some() {
                config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
                config["startup_timeout_ms"] = json!(30000);
                config["request_timeout_ms"] = json!(30000);
            }
            for model in config["models"].as_array_mut().unwrap() {model["policy"]=json!("disabled");}
            let root=Path::new(config["models"][0]["model_file"].as_str().unwrap()).parent().unwrap().join("native runtime bundle");
            std::fs::create_dir(&root).unwrap();
            #[cfg(target_os = "linux")]
            if let Some(engine) = real_engine {
                runtime=Some(flat_cpu_bundle(engine, &root));
                return;
            }
            let name=format!("juiced-test-worker{}",std::env::consts::EXE_SUFFIX);
            let executable=root.join(&name);
            std::fs::copy(binary("juiced-test-worker","JUICED_TEST_WORKER"),&executable).unwrap();
            let checksum=digest(&executable);
            runtime=Some((executable,json!({"schema_version":1,"implementation_id":"llama-cpp","root":root,"entrypoint":name,"library_directories":[],"files":[{"path":name,"sha256":checksum,"role":"executable"}]})));
        }).await;
        let download = if real_engine.is_some() {
            let source = Source::gated_with_bytes(weights, 0).await;
            source.finish();
            source
        } else {
            Source::with_bytes(weights).await
        };
        let inventory = server.get("juiced/artifacts").await;
        let (status,started)=server.post("juiced/artifacts/pull",json!({"expected_generation":inventory["generation"],"download":download.download()})).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let job = format!("juiced/artifacts/jobs/{}", started["id"].as_str().unwrap());
        server
            .wait_for(async || {
                let job = server.get(&job).await;
                job["state"] == "complete" && job["writer_active"] == false
            })
            .await;
        let artifact = server.get(&job).await["artifact"]
            .as_str()
            .unwrap()
            .to_owned();
        let source_owner = tempfile::tempdir().unwrap();
        let source = source_owner.path().join("copied native source");
        std::fs::create_dir(&source).unwrap();
        for name in [
            "config.json",
            "jobs.json",
            "native_helpers.json",
            "execution_profiles.json",
        ] {
            std::fs::write(source.join(name), b"{}\n").unwrap();
        }
        std::fs::write(source.join("user_models.json"),serde_json::to_vec_pretty(&json!({"Tiny":{"recipe":"llamacpp","checkpoint":"copied-native-checkpoint.gguf","recipe_options":{"ctx_size":512,"llamacpp_backend":"cpu"},"labels":["chat"]},"OpaqueFuture":{"unknown":[1,2,{"private":SOURCE_MARKER}]}})).unwrap()).unwrap();
        std::fs::write(source.join("recipe_options.json"),serde_json::to_vec_pretty(&json!({MODEL:{"ctx_size":768},"opaque-unrelated":{"private":SOURCE_MARKER,"future":[1,{"two":2}]}})).unwrap()).unwrap();
        std::fs::set_permissions(
            source.join("recipe_options.json"),
            std::os::unix::fs::PermissionsExt::from_mode(0o640),
        )
        .unwrap();
        std::fs::write(
            source.join("aliases.json"),
            b"{\"friendly\":\"user.Tiny\"}\n",
        )
        .unwrap();
        std::fs::write(source.join("untouched-operator-note"), SOURCE_MARKER).unwrap();
        let (command, bundle) = runtime.unwrap();
        let binding = json!({"id":MODEL,"artifact":artifact,"model_file":"ignored-native-binding-path","execution_profile":{"schema_version":2,"native_model_id":MODEL,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_bundle":bundle,"command":command,"args":["--host","127.0.0.1","--port","{port}","--model","{model}","--slot-save-path","{cache}"],"placement":"unresolved"},"resident_bytes":64,"max_inflight":1,"essential_headroom":0,"operations":["completions"],"policy":"disabled","revision":0});
        let mut binding = binding;
        if real_engine.is_some() {
            binding["resident_bytes"] = json!(512u64 * 1024 * 1024);
        }
        let bindings = source.join("explicit native bindings.json");
        std::fs::write(
            &bindings,
            serde_json::to_vec(&vec![binding.clone()]).unwrap(),
        )
        .unwrap();
        Self {
            server,
            _source: source_owner,
            source,
            bindings,
            binding,
            artifact,
        }
    }
    fn write_binding(&self, binding: &Value) {
        std::fs::write(&self.bindings, serde_json::to_vec(&vec![binding]).unwrap()).unwrap();
    }
    fn source_image(&self) -> (BTreeMap<PathBuf, Entry>, BTreeMap<PathBuf, u32>) {
        use std::os::unix::fs::PermissionsExt;
        let image = tree(&self.source);
        let modes = image
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    std::fs::metadata(self.source.join(path))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o7777,
                )
            })
            .collect();
        (image, modes)
    }
    async fn import(&self) -> Output {
        async fn capture(pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
            let mut output = Vec::new();
            pipe.take(65537).read_to_end(&mut output).await.unwrap();
            output
        }
        let mut command = tokio::process::Command::new(binary("lemond", "JUICED_TEST_SERVER"));
        command
            .arg("--config")
            .arg(&self.server.config_path)
            .arg("--import-native-settings-copy")
            .arg(&self.source)
            .arg("--native-bindings")
            .arg(&self.bindings)
            .arg("--include-native-aliases")
            .env_remove("LEMONADE_API_KEY")
            .env_remove("LEMONADE_ADMIN_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let stdout = tokio::spawn(capture(child.stdout.take().unwrap()));
        let stderr = tokio::spawn(capture(child.stderr.take().unwrap()));
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("offline native import exceeded its process deadline")
            .unwrap();
        let output = Output {
            status,
            stdout: stdout.await.unwrap(),
            stderr: stderr.await.unwrap(),
        };
        assert!(output.stdout.len() <= 65536 && output.stderr.len() <= 65536);
        for bytes in [&output.stdout, &output.stderr] {
            assert!(
                !bytes
                    .windows(SOURCE_MARKER.len())
                    .any(|part| part == SOURCE_MARKER.as_bytes()),
                "offline diagnostics exposed native document content"
            );
            assert!(
                !String::from_utf8_lossy(bytes).contains("listening on"),
                "offline import opened a public listener"
            );
        }
        output
    }
    fn disk(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.server.config_path).unwrap()).unwrap()
    }
    fn blob(&self, disk: &Value, original: bool, role: &str) -> Vec<u8> {
        let generation = if original { "original" } else { "current" };
        let entry = &disk["native_settings"][generation]["files"][role];
        assert_eq!(entry["state"], "blob");
        std::fs::read(
            self.server
                .directory
                .path()
                .join(STORE_ROOT)
                .join(format!("blob-{}", entry["id"].as_str().unwrap())),
        )
        .unwrap()
    }
    fn no_pending_generations(&self) {
        let root = self.server.directory.path().join(STORE_ROOT);
        if root.exists() {
            assert!(!std::fs::read_dir(root).unwrap().any(|entry| {
                let name = entry.unwrap().file_name();
                let name = name.to_string_lossy();
                name.starts_with("intent-") || name.starts_with("blob-")
            }));
        }
    }
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "offline import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn rejected(output: Output, code: Option<&str>) {
    assert!(!output.status.success(), "invalid offline import succeeded");
    assert!(output.stdout.is_empty());
    if let Some(code) = code {
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(code),
            "offline failure omitted expected safe code {code}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
async fn complete(server: &Server) -> Value {
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", "native-import-session")
        .json(&json!({"model":MODEL,"prompt":"Native data pipeline fixture"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"], MODEL);
    assert!(body["received_body_sha256"].is_string());
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    body
}
async fn unload(server: &Server) {
    let (status, result) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
}

const HOST_RESIDENT: u64 = 8 * 1024 * 1024;
const HOST_REQUEST: u64 = 2 * 1024 * 1024;
#[cfg(target_os = "linux")]
const HOST_INGRESS: u64 = 64 * 2 * 1024 * 1024;

async fn protected_baseline() -> (Inputs, String) {
    protected_baseline_with(Inputs::prepare().await).await
}

async fn protected_baseline_with(mut inputs: Inputs) -> (Inputs, String) {
    inputs.binding["operations"] = json!(["chat/completions", "completions", "responses"]);
    inputs.write_binding(&inputs.binding);
    inputs.server.stop();
    success(inputs.import().await);
    inputs.server.start().await;
    let revision = inputs.server.model(MODEL).await["revision"].clone();
    let (status, enabled) = inputs
        .server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"on-demand","expected_revision":revision}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let context = inputs
        .server
        .get(&format!("juiced/context?model={MODEL}"))
        .await;
    let fingerprint = context["applied"]["launch_plan"]["semantic_fingerprint"]
        .as_str()
        .expect("baseline retained launch identity")
        .to_owned();
    assert_eq!(fingerprint.len(), 64);
    unload(&inputs.server).await;
    inputs.server.stop();
    (inputs, fingerprint)
}

fn protected_config(
    inputs: &Inputs,
    fingerprint: &str,
    capacity: Option<u64>,
    completion_ms: u64,
) -> Value {
    let mut config = inputs.disk();
    config["resident_budget_bytes"] = json!(1);
    config["request_timeout_ms"] = json!(10000);
    config["admission"]["memory"] = json!({"mode":"protected","capacity_bytes":capacity,"essential_headroom_bytes":0,"maximum_sample_age_ms":5000});
    let model = config["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["id"] == MODEL)
        .unwrap();
    assert!(model["resident_bytes"].as_u64().unwrap() > 1);
    model["resource_profile"] = json!({"schema_version":1,"launch_fingerprint":fingerprint,"priority":"interactive","completion_ms":completion_ms,"load_bytes":16*1024*1024,"resident_bytes":HOST_RESIDENT,"request_bytes":HOST_REQUEST,"state_bytes":null,"request_limits":{"input_bytes":65536,"batch":1,"output_tokens":8}});
    config
}

fn write_config(inputs: &Inputs, config: &Value) {
    std::fs::write(
        &inputs.server.config_path,
        serde_json::to_vec_pretty(config).unwrap(),
    )
    .unwrap();
}

fn cache_control_claims(admission: &Value) -> (u64, u64, u64) {
    let control = &admission["resources"]["cache_control_memory"];
    let resident = control["metadata_bytes"].as_u64().unwrap();
    let scratch = control["scratch_bytes"].as_u64().unwrap();
    let reserved = control["reserved_bytes"].as_u64().unwrap();
    assert_eq!(reserved, resident + scratch);
    assert_eq!(control["outstanding_bytes"], reserved);
    if control["state"] == "accounted" {
        assert!(resident > 0);
        assert_eq!(control["bounds"]["metadata_bytes"], resident);
        let active = control["active_operations"].as_u64().unwrap();
        assert!(scratch >= active * control["bounds"]["page_scratch_bytes"].as_u64().unwrap());
        assert!(scratch <= active * control["bounds"]["scratch_bytes"].as_u64().unwrap());
    } else {
        assert_eq!(control["state"], "unavailable");
        assert_eq!(reserved, 0);
        assert_eq!(control["active_operations"], 0);
        assert!(control["reason"].as_str().is_some());
    }
    (resident, scratch, reserved)
}

async fn host_claims(server: &Server, resident: u64, request: u64, ingress: u64) -> Value {
    let total = resident + request + ingress;
    let outstanding = request + ingress;
    let settled = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let admission = server.get("juiced/admission").await;
            let memory = &admission["scheduler"]["memory"];
            let (control_resident, control_scratch, control_reserved) =
                cache_control_claims(&admission);
            if memory["reserved_bytes"] == total + control_reserved
                && memory["outstanding_bytes"] == outstanding + control_resident + control_scratch
                && memory["resident_bytes"] == resident
            {
                assert_eq!(
                    server.model(MODEL).await["request_limits"],
                    json!({"input_bytes":65536,"batch":1,"output_tokens":8})
                );
                return admission;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "host claims did not reach resident={resident}, request={request}, ingress={ingress}; observed={}",
        server.get("juiced/admission").await
    );
    let admission = settled.unwrap();
    let resources = &admission["resources"];
    let memory = &admission["scheduler"]["memory"];
    assert_eq!(resources["reservation_source"], "declared-protected-host");
    assert_eq!(resources["resident_budget_bytes"], memory["capacity_bytes"]);
    assert_eq!(resources["reserved_bytes"], memory["reserved_bytes"]);
    assert_eq!(memory["pool"], "host");
    assert_eq!(server.model(MODEL).await["resident_bytes"], HOST_RESIDENT);
    admission
}

async fn protected_suitability_is_read_only(inputs: &Inputs) {
    fn without_clock_derived_memory(mut admission: Value) -> Value {
        let memory = admission["scheduler"]["memory"].as_object_mut().unwrap();
        for key in ["sample_age_ms", "sampled_available_bytes", "capacity_state"] {
            memory.remove(key);
        }
        admission
    }
    let bytes = std::fs::read(&inputs.server.config_path).unwrap();
    let catalog = inputs.server.get("juiced/catalog").await;
    let records = inputs.server.get("juiced/requests").await;
    let admission = inputs.server.get("juiced/admission").await;
    let memory = &admission["scheduler"]["memory"];
    let resources = inputs.server.get("juiced/state").await["resources"].clone();
    assert_eq!(resources["reservation_source"], "declared-protected-host");
    assert_eq!(resources["resident_budget_bytes"], memory["capacity_bytes"]);
    assert_eq!(resources["reserved_bytes"], memory["reserved_bytes"]);
    assert_eq!(
        inputs.server.model(MODEL).await["resident_bytes"],
        HOST_RESIDENT
    );
    assert!(inputs.server.model("fixture.one").await["resident_bytes"].is_null());
    for prefix in PREFIXES {
        let (status, suitability) = inputs
            .server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/suitability?model={MODEL}"),
                None,
                inputs.server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{suitability}");
        assert_eq!(suitability["read_only"], true);
        assert_eq!(suitability["reservation"], false);
        assert_eq!(suitability["contract_scope"], "declared-protected-host");
        assert_eq!(suitability["resources"]["fit"], "unknown");
        assert_eq!(suitability["resources"]["hardware_qualified"], false);
        assert_eq!(suitability["resources"]["recommendation_qualified"], false);
        assert_eq!(
            suitability["resources"]["reservation_source"],
            "declared-protected-host"
        );
        let budget = &suitability["resources"]["declared_budget"];
        assert_eq!(budget["capacity_bytes"], memory["capacity_bytes"]);
        assert_eq!(budget["reserved_bytes"], memory["reserved_bytes"]);
        assert_eq!(budget["additional_worker_bytes"], 16 * 1024 * 1024);
        if memory["capacity_bytes"].is_null() || budget["available_bytes"].is_null() {
            assert!(budget["available_bytes"].is_null());
            assert!(budget["fits_additional_reservation"].is_null());
        } else {
            assert!(
                budget["available_bytes"].as_u64().unwrap()
                    <= memory["capacity_bytes"].as_u64().unwrap()
            );
            assert!(budget["fits_additional_reservation"].is_boolean());
        }
    }
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), bytes);
    assert_eq!(inputs.server.get("juiced/catalog").await, catalog);
    assert_eq!(inputs.server.get("juiced/requests").await, records);
    assert_eq!(
        inputs.server.get("juiced/state").await["resources"],
        resources
    );
    assert_eq!(
        without_clock_derived_memory(inputs.server.get("juiced/admission").await),
        without_clock_derived_memory(admission)
    );
}

async fn no_protected_child(server: &Server) {
    let admission = host_claims(server, 0, 0, 0).await;
    let model = server.model(MODEL).await;
    assert!(model["pid"].is_null(), "{model}");
    assert!(model["process_lifecycle"].is_null(), "{model}");
    assert_eq!(model["loaded"], false);
    assert_eq!(model["active_requests"], 0);
    assert_eq!(model["uncertain_requests"], 0);
    assert_eq!(admission["processes"], json!([]));
    assert_eq!(admission["resources"]["power"]["active"], 0);
    let (controller_resident, controller_scratch, _) = cache_control_claims(&admission);
    assert_eq!(controller_scratch, 0);
    assert_eq!(
        admission["resources"]["reserved_bytes"],
        controller_resident
    );
    assert_eq!(admission["ingress"]["reserved_bytes"], 0);
}

#[tokio::test]
async fn protected_unknown_host_capacity_refuses_work_without_children_or_mutations() {
    let (mut inputs, fingerprint) = protected_baseline().await;
    let config = protected_config(&inputs, &fingerprint, None, 5000);
    write_config(&inputs, &config);
    let bytes = std::fs::read(&inputs.server.config_path).unwrap();
    let source = inputs.source_image();
    let artifacts = tree(&inputs.server.directory.path().join("artifacts"));
    let native = tree(&inputs.server.directory.path().join(STORE_ROOT));
    inputs.server.start().await;
    protected_suitability_is_read_only(&inputs).await;
    let records = inputs.server.get("juiced/requests").await;
    let (status, error) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert_eq!(error["error"]["code"], "unknown_host_capacity");
    for prefix in PREFIXES {
        let path = format!("{prefix}/completions");
        let (status, error) = inputs
            .server
            .json(
                Method::POST,
                &path,
                Some(json!({"model":MODEL,"prompt":"bounded text","max_tokens":4})),
                inputs.server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
        assert_eq!(error["error"]["code"], "unknown_host_capacity");
        let (status, error) = inputs
            .server
            .json(
                Method::POST,
                &path,
                Some(json!({"model":MODEL,"prompt":"bounded text","max_tokens":4})),
                Some("wrong-test-credential"),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
    }
    no_protected_child(&inputs.server).await;
    protected_suitability_is_read_only(&inputs).await;
    assert_eq!(inputs.server.get("health").await["status"], "ok");
    assert_eq!(inputs.server.get("juiced/requests").await, records);
    assert_eq!(
        inputs.server.get("juiced/admission").await["scheduler"]["memory"]["capacity_state"],
        "unknown"
    );
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), bytes);
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(&inputs.server.directory.path().join("artifacts")),
        artifacts
    );
    assert_eq!(
        tree(&inputs.server.directory.path().join(STORE_ROOT)),
        native
    );
}

#[tokio::test]
async fn invalid_protected_profiles_refuse_startup_without_rewriting_imported_state() {
    let (inputs, fingerprint) = protected_baseline().await;
    let source = inputs.source_image();
    let artifacts = tree(&inputs.server.directory.path().join("artifacts"));
    let native = tree(&inputs.server.directory.path().join(STORE_ROOT));
    for defect in ["identity", "phase-ceiling", "deadline", "input-limit"] {
        let mut config = protected_config(&inputs, &fingerprint, Some(256 * 1024 * 1024), 5000);
        let profile = &mut config["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["id"] == MODEL)
            .unwrap()["resource_profile"];
        match defect {
            "identity" => profile["launch_fingerprint"] = json!("A".repeat(64)),
            "phase-ceiling" => profile["load_bytes"] = json!(HOST_RESIDENT - 1),
            "deadline" => profile["completion_ms"] = json!(300001),
            _ => profile["request_limits"]["input_bytes"] = json!(0),
        }
        write_config(&inputs, &config);
        let bytes = std::fs::read(&inputs.server.config_path).unwrap();
        let result = support::rejected_manager(&inputs.server.config_path).await;
        assert!(!result.status.success(), "{defect} started a manager");
        assert!(result.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("configuration_recovery_required") && stderr.contains("decode"),
            "{stderr}"
        );
        assert!(!stderr.contains(SOURCE_MARKER));
        assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), bytes);
        assert_eq!(inputs.source_image(), source);
        assert_eq!(
            tree(&inputs.server.directory.path().join("artifacts")),
            artifacts
        );
        assert_eq!(
            tree(&inputs.server.directory.path().join(STORE_ROOT)),
            native
        );
    }
}

#[cfg(target_os = "linux")]
async fn protected_completion(server: &Server, operation: &str, mut body: Value) {
    body["model"] = json!(MODEL);
    let mut normalized = body.clone();
    normalized.as_object_mut().unwrap().remove("priority");
    normalized.as_object_mut().unwrap().remove("logprobs");
    for alias in ["n_predict", "max_completion_tokens"] {
        if let Some(value) = normalized.as_object_mut().unwrap().remove(alias) {
            normalized["max_tokens"] = value;
        }
    }
    normalized.as_object_mut().unwrap().remove("n");
    normalized["n_cmpl"] = json!(1);
    normalized["n_probs"] = json!(0);
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&normalized).unwrap())
    );
    let response = server
        .client
        .post(format!("{}/v1/{operation}", server.base))
        .bearer_auth(server.read_key().unwrap())
        .header("x-juiced-priority", "batch")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let result: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["received_body_sha256"], digest);
    assert_eq!(result["model"], MODEL);
    assert_eq!(result["received_authorization"], false);
    assert_eq!(result["manager_key_present"], false);
    assert_eq!(result["choices"].as_array().unwrap().len(), 1);
    assert!(
        result["usage"]["completion_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    host_claims(server, HOST_RESIDENT, 0, 0).await;
    let model = server.model(MODEL).await;
    assert_eq!(model["active_requests"], 0);
    assert_eq!(model["uncertain_requests"], 0);
}

#[cfg(target_os = "linux")]
async fn chunked_protected_stream(server: &Server) -> tokio::net::TcpStream {
    use tokio::io::AsyncWriteExt;
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut socket=tokio::net::TcpStream::connect(server.base.trim_start_matches("http://")).await.unwrap();
        let body=serde_json::to_vec(&json!({"model":MODEL,"prompt":"whole stream EOF","max_tokens":4,"stream":true})).unwrap();
        let header=format!("POST /v1/completions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",server.read_key().unwrap());
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(format!("{:x}\r\n",body.len()).as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        let mut header=Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len()<8192);
            let mut byte=[0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
        }
        let header=std::str::from_utf8(&header).unwrap();
        if !header.starts_with("HTTP/1.1 200 ") {
            let mut body=Vec::new();
            let _=tokio::time::timeout(Duration::from_secs(1),(&mut socket).take(8193).read_to_end(&mut body)).await;
            panic!("chunked protected SSE rejected: {header}; body={}",String::from_utf8_lossy(&body));
        }
        assert!(header.to_ascii_lowercase().contains("content-type: text/event-stream"));
        socket
    }).await.expect("chunked protected SSE did not return bounded response headers")
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_HOST_MEMORY=1 and a complete known host-memory sample; selected fixture HTTP lifecycle"]
async fn protected_host_claims_normalize_text_refund_original_eof_and_retain_disconnect_until_explicit_unload()
 {
    assert_eq!(
        std::env::var("JUICED_TEST_HOST_MEMORY").as_deref(),
        Ok("1"),
        "set JUICED_TEST_HOST_MEMORY=1 for the actual host-memory pipeline"
    );
    let (mut inputs, fingerprint) = protected_baseline().await;
    let source = inputs.source_image();
    let native = tree(&inputs.server.directory.path().join(STORE_ROOT));
    let artifacts = tree(&inputs.server.directory.path().join("artifacts"));
    let wrong = if fingerprint == "b".repeat(64) {
        "c".repeat(64)
    } else {
        "b".repeat(64)
    };
    let config = protected_config(&inputs, &wrong, Some(256 * 1024 * 1024), 5000);
    write_config(&inputs, &config);
    inputs.server.start().await;
    let (status, error) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "wrong launch identity must refuse before spawn: {error}; host={}",
        inputs.server.get("juiced/state").await["resources"]["host_memory"]
    );
    assert_eq!(error["error"]["code"], "admission_profile_mismatch");
    no_protected_child(&inputs.server).await;
    assert_eq!(inputs.disk(), config);
    inputs.server.stop();
    let config = protected_config(&inputs, &fingerprint, Some(256 * 1024 * 1024), 5000);
    write_config(&inputs, &config);
    let bytes = std::fs::read(&inputs.server.config_path).unwrap();
    inputs.server.start().await;
    for prefix in PREFIXES {
        for (operation, body, status, code) in [
            (
                "completions",
                json!({"model":MODEL,"prompt":"text","max_tokens":4,"n_predict":5}),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "completions",
                json!({"model":MODEL,"prompt":"text","max_tokens":9}),
                StatusCode::PAYLOAD_TOO_LARGE,
                "workload_limit_exceeded",
            ),
            (
                "completions",
                json!({"model":MODEL,"prompt":"x".repeat(12000),"max_tokens":4}),
                StatusCode::PAYLOAD_TOO_LARGE,
                "workload_limit_exceeded",
            ),
            (
                "completions",
                json!({"model":MODEL,"prompt":"text","max_tokens":4,"extra_params":{"n_predict":100}}),
                StatusCode::SERVICE_UNAVAILABLE,
                "workload_unqualified",
            ),
            (
                "completions",
                json!({"model":MODEL,"prompt":"text","max_tokens":4,"id_slot":0}),
                StatusCode::SERVICE_UNAVAILABLE,
                "workload_unqualified",
            ),
            (
                "chat/completions",
                json!({"model":MODEL,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"private test input"}}]}],"max_tokens":4}),
                StatusCode::SERVICE_UNAVAILABLE,
                "workload_unqualified",
            ),
            (
                "responses",
                json!({"model":MODEL,"input":"text","max_output_tokens":4,"tools":[]}),
                StatusCode::SERVICE_UNAVAILABLE,
                "workload_unqualified",
            ),
        ] {
            let (actual, error) = inputs
                .server
                .json(
                    Method::POST,
                    &format!("{prefix}/{operation}"),
                    Some(body),
                    inputs.server.read_key(),
                )
                .await;
            assert_eq!(actual, status, "{error}");
            assert_eq!(error["error"]["code"], code);
            no_protected_child(&inputs.server).await;
            assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), bytes);
        }
    }
    assert_eq!(
        inputs.server.get("juiced/requests").await["data"],
        json!([])
    );
    let cache = inputs.server.get("juiced/cache").await;
    let (status,error)=inputs.server.post("juiced/cache/save",json!({"model":MODEL,"slot":0,"session":"protected-state-rejected","expected_generation":cache["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert_eq!(error["error"]["code"], "state_work_unqualified");
    no_protected_child(&inputs.server).await;
    assert_eq!(inputs.server.get("juiced/cache").await, cache);
    let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{loaded}; host={}",
        inputs.server.get("juiced/state").await["resources"]["host_memory"]
    );
    let resident = host_claims(&inputs.server, HOST_RESIDENT, 0, 0).await;
    protected_suitability_is_read_only(&inputs).await;
    let sample = resident["resources"]["host_memory"].clone();
    assert_eq!(sample["constraints_complete"], true, "{sample}");
    assert!(sample["reason"].is_null(), "{sample}");
    assert!(
        sample["available_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes >= HOST_INGRESS + HOST_RESIDENT + HOST_REQUEST),
        "{sample}"
    );
    let first_model = inputs.server.model(MODEL).await;
    let pid = first_model["pid"].as_u64().unwrap();
    let process = first_model["process_lifecycle"].clone();
    protected_completion(&inputs.server,"completions",json!({"prompt":"normalized text","n_predict":4,"n":1,"priority":"essential","logprobs":false,"cache_prompt":true})).await;
    protected_completion(&inputs.server,"chat/completions",json!({"messages":[{"role":"user","content":[{"type":"text","text":"bounded chat"}]}],"max_completion_tokens":4})).await;
    protected_completion(
        &inputs.server,
        "responses",
        json!({"input":"bounded response","max_output_tokens":4}),
    )
    .await;
    assert_eq!(inputs.server.model(MODEL).await["pid"], pid);
    let response = chunked_protected_stream(&inputs.server).await;
    host_claims(&inputs.server, HOST_RESIDENT, HOST_REQUEST, HOST_INGRESS).await;
    let mut whole = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(8),
        response.take(65537).read_to_end(&mut whole),
    )
    .await
    .expect("protected SSE original EOF deadline")
    .unwrap();
    assert!(!whole.is_empty() && whole.len() < 65536);
    host_claims(&inputs.server, HOST_RESIDENT, 0, 0).await;
    let completed = inputs.server.model(MODEL).await;
    assert_eq!(completed["pid"], pid);
    assert_eq!(completed["active_requests"], 0);
    assert_eq!(completed["uncertain_requests"], 0);
    let revision = completed["revision"].clone();
    let (status, hot) = inputs
        .server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"hot","expected_revision":revision}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{hot}");
    let saved = std::fs::read(&inputs.server.config_path).unwrap();
    let mut response=inputs.server.client.post(format!("{}/v1/completions",inputs.server.base)).bearer_auth(inputs.server.read_key().unwrap()).json(&json!({"model":MODEL,"prompt":"disconnect after first stream frame","max_tokens":4,"stream":true})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chunk = response
        .chunk()
        .await
        .unwrap()
        .expect("first upstream stream frame");
    assert!(!chunk.is_empty());
    drop(response);
    inputs
        .server
        .wait_for(async || inputs.server.model(MODEL).await["uncertain_requests"] == 1)
        .await;
    let retained = host_claims(&inputs.server, HOST_RESIDENT, HOST_REQUEST, 0).await;
    protected_suitability_is_read_only(&inputs).await;
    let uncertain = inputs.server.model(MODEL).await;
    assert_eq!(uncertain["pid"], pid);
    assert_eq!(uncertain["process_lifecycle"], process);
    assert_eq!(uncertain["policy"], "hot");
    assert_eq!(uncertain["active_requests"], 1);
    assert_eq!(uncertain["state"], "cleanup-pending");
    assert_eq!(uncertain["cleanup_retry_available"], true);
    assert_eq!(retained["resources"]["power"]["active"], 1);
    assert_eq!(retained["scheduler"]["active"], 1);
    assert!(Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(inputs.server.get("health").await["status"], "ok");
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), saved);
    let (status, cleaned) = inputs.server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{cleaned}");
    assert_eq!(cleaned["policy"], "hot");
    no_protected_child(&inputs.server).await;
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), saved);
    let (status, reloaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    let replacement = inputs.server.model(MODEL).await;
    assert_ne!(replacement["pid"], pid);
    assert_eq!(replacement["policy"], "hot");
    assert!(
        replacement["process_lifecycle"]["generation"]
            .as_u64()
            .unwrap()
            > process["generation"].as_u64().unwrap()
    );
    protected_completion(
        &inputs.server,
        "completions",
        json!({"prompt":"after acknowledged cleanup","max_tokens":4}),
    )
    .await;
    let (status, on_demand) = inputs
        .server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"on-demand","expected_revision":replacement["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{on_demand}");
    unload(&inputs.server).await;
    no_protected_child(&inputs.server).await;
    assert!(!Path::new(&format!("/proc/{}", replacement["pid"].as_u64().unwrap())).exists());
    inputs.server.stop();
    let mut timeout_config = inputs.disk();
    let timed_model = timeout_config["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["id"] == MODEL)
        .unwrap();
    timed_model["policy"] = json!("hot");
    timed_model["resource_profile"]["completion_ms"] = json!(500);
    assert_eq!(
        timed_model["resource_profile"]["launch_fingerprint"],
        fingerprint
    );
    write_config(&inputs, &timeout_config);
    let timeout_saved = std::fs::read(&inputs.server.config_path).unwrap();
    inputs.server.start().await;
    let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    host_claims(&inputs.server, HOST_RESIDENT, 0, 0).await;
    let timeout_model = inputs.server.model(MODEL).await;
    let timeout_pid = timeout_model["pid"].as_u64().unwrap();
    assert_eq!(timeout_model["policy"], "hot");
    let response=inputs.server.client.post(format!("{}/v1/completions",inputs.server.base)).bearer_auth(inputs.server.read_key().unwrap()).json(&json!({"model":MODEL,"prompt":"upstream deadline before original EOF","max_tokens":4,"stream":true})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.bytes().await.is_err(),
        "the500ms upstream deadline must interrupt the three-second original SSE body"
    );
    inputs
        .server
        .wait_for(async || inputs.server.model(MODEL).await["uncertain_requests"] == 1)
        .await;
    let timeout_retained = host_claims(&inputs.server, HOST_RESIDENT, HOST_REQUEST, 0).await;
    let timed_out = inputs.server.model(MODEL).await;
    assert_eq!(timed_out["pid"], timeout_pid);
    assert_eq!(
        timed_out["process_lifecycle"],
        timeout_model["process_lifecycle"]
    );
    assert_eq!(timed_out["policy"], "hot");
    assert_eq!(timed_out["active_requests"], 1);
    assert_eq!(timed_out["state"], "cleanup-pending");
    assert_eq!(timed_out["cleanup_retry_available"], true);
    assert_eq!(timeout_retained["resources"]["power"]["active"], 1);
    assert_eq!(timeout_retained["scheduler"]["active"], 1);
    assert!(Path::new(&format!("/proc/{timeout_pid}")).exists());
    assert_eq!(
        std::fs::read(&inputs.server.config_path).unwrap(),
        timeout_saved
    );
    let records = inputs.server.get("juiced/requests").await;
    let timeout_record = records["data"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(timeout_record["model"], MODEL);
    assert_eq!(timeout_record["outcome"], "timeout");
    let (status, cleaned) = inputs.server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{cleaned}");
    assert_eq!(cleaned["policy"], "hot");
    no_protected_child(&inputs.server).await;
    assert!(!Path::new(&format!("/proc/{timeout_pid}")).exists());
    assert_eq!(
        std::fs::read(&inputs.server.config_path).unwrap(),
        timeout_saved
    );
    let (status, reloaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    let timeout_replacement = inputs.server.model(MODEL).await;
    assert_ne!(timeout_replacement["pid"], timeout_pid);
    assert_eq!(timeout_replacement["policy"], "hot");
    assert!(
        timeout_replacement["process_lifecycle"]["generation"]
            .as_u64()
            .unwrap()
            > timeout_model["process_lifecycle"]["generation"]
                .as_u64()
                .unwrap()
    );
    protected_completion(
        &inputs.server,
        "completions",
        json!({"prompt":"after timeout exit acknowledgement","max_tokens":4}),
    )
    .await;
    let (status,on_demand)=inputs.server.post("juiced/policy",json!({"model":MODEL,"policy":"on-demand","expected_revision":timeout_replacement["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{on_demand}");
    unload(&inputs.server).await;
    no_protected_child(&inputs.server).await;
    assert!(
        !Path::new(&format!(
            "/proc/{}",
            timeout_replacement["pid"].as_u64().unwrap()
        ))
        .exists()
    );
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(&inputs.server.directory.path().join(STORE_ROOT)),
        native
    );
    assert_eq!(
        tree(&inputs.server.directory.path().join("artifacts")),
        artifacts
    );
    let manager = inputs.server.pid();
    inputs.server.stop();
    assert!(!Path::new(&format!("/proc/{manager}")).exists());
    eprintln!(
        "protected-host-receipt {}",
        json!({"schema_version":1,"scope":"selected imported fixture CPU text pipeline with operator-declared host ceilings and an actual complete memory sample; no real-engine peak calibration","sample":sample,"profile":config["models"].as_array().unwrap().iter().find(|model|model["id"]==MODEL).unwrap()["resource_profile"],"normal_eof_refunded":true,"disconnect_retained":{"reserved_bytes":retained["scheduler"]["memory"]["reserved_bytes"],"outstanding_bytes":retained["scheduler"]["memory"]["outstanding_bytes"],"resident_bytes":retained["scheduler"]["memory"]["resident_bytes"],"active_requests":uncertain["active_requests"],"uncertain_requests":uncertain["uncertain_requests"]},"timeout_retained":{"completion_ms":500,"body_read_failed":true,"reserved_bytes":timeout_retained["scheduler"]["memory"]["reserved_bytes"],"outstanding_bytes":timeout_retained["scheduler"]["memory"]["outstanding_bytes"],"resident_bytes":timeout_retained["scheduler"]["memory"]["resident_bytes"],"active_requests":timed_out["active_requests"],"uncertain_requests":timed_out["uncertain_requests"],"request_record":timeout_record},"protected_worker_pids":[pid,replacement["pid"],timeout_pid,timeout_replacement["pid"]],"saved_hot_policy_preserved_during_cleanup":true,"protected_worker_exit_count":4,"final_manager_exit_acknowledged":true,"final_host_claims":0,"source_unchanged":true})
    );
}

#[tokio::test]
async fn exclusive_offline_copy_import_preserves_source_and_drives_managed_completion_cache_and_native_context_edits()
 {
    let mut inputs = Inputs::prepare().await;
    let source = inputs.source_image();
    let original = std::fs::read(&inputs.server.config_path).unwrap();
    let artifacts = tree(&inputs.server.directory.path().join("artifacts"));
    rejected(inputs.import().await, None);
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), original);
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(&inputs.server.directory.path().join("artifacts")),
        artifacts
    );
    assert_eq!(inputs.server.get("health").await["status"], "ok");
    inputs.server.stop();
    let result = success(inputs.import().await);
    assert_eq!(result["models_added"], 1);
    assert_eq!(result["source_modified"], false);
    assert_eq!(result["imported_policy"], "disabled");
    let imported = inputs.disk();
    assert_eq!(imported["schema_version"], 3);
    assert_eq!(imported["native_settings"]["schema_version"], 1);
    assert_eq!(
        imported["native_settings"]["original"],
        imported["native_settings"]["current"]
    );
    assert_eq!(
        imported["native_settings"]["current"]["files"]["aliases"]["state"],
        "imported"
    );
    let model = imported["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == MODEL)
        .unwrap();
    assert_eq!(model["policy"], "disabled");
    assert_eq!(model["execution_profile"]["schema_version"], 2);
    assert!(model["execution_profile"].get("options").is_none());
    let managed = inputs
        .server
        .directory
        .path()
        .join("artifacts")
        .join(format!("artifact-{}.bin", inputs.artifact));
    assert_eq!(
        Path::new(model["model_file"].as_str().unwrap()),
        std::fs::canonicalize(&managed).unwrap()
    );
    assert_eq!(
        inputs.blob(&imported, true, "recipe_options"),
        std::fs::read(inputs.source.join("recipe_options.json")).unwrap()
    );
    let config = std::fs::read(&inputs.server.config_path).unwrap();
    rejected(
        inputs.import().await,
        Some("This configuration already owns imported native settings"),
    );
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), config);
    inputs.server.start().await;
    assert!(inputs.server.model(MODEL).await["pid"].is_null());
    assert_eq!(
        inputs.server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    let saved = inputs.server.get(&format!("models/{MODEL}/options")).await;
    assert_eq!(
        saved["saved"],
        json!({"context_tokens":768,"threads":null,"gpu_layers":null,"parallel_slots":null})
    );
    assert_eq!(saved["options_authority"], "native-settings");
    assert_eq!(saved["editable_fields"], json!(["context_tokens"]));
    assert_eq!(saved["effective_launch"]["context_tokens"], 768);
    assert_eq!(saved["effective_launch"]["gpu_layers"], 0);
    assert_eq!(saved["effective_launch"]["parallel_slots"], 1);
    let revision = inputs.server.model(MODEL).await["revision"].clone();
    let unchanged = inputs.disk();
    for options in [
        json!({"threads":2}),
        json!({"gpu_layers":0}),
        json!({"parallel_slots":1}),
    ] {
        let (status, error) = inputs
            .server
            .post(
                &format!("models/{MODEL}/options"),
                json!({"expected_revision":revision,"options":options}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "native_options_authority");
        assert_eq!(inputs.disk(), unchanged);
    }
    for prefix in PREFIXES {
        let (status, read) = inputs
            .server
            .json(
                Method::GET,
                &format!("{prefix}/models/{MODEL}/options"),
                None,
                inputs.server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read, saved);
        let (status,error)=inputs.server.json(Method::POST,&format!("{prefix}/models/{MODEL}/options"),Some(json!({"expected_revision":revision.as_u64().unwrap()+99,"options":{"context_tokens":1024}})),inputs.server.admin_key()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "revision_conflict");
    }
    let (status, enabled) = inputs
        .server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"on-demand","expected_revision":revision}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    complete(&inputs.server).await;
    let context = inputs
        .server
        .get(&format!("juiced/context?model={MODEL}"))
        .await;
    let first_fingerprint = context["applied"]["execution_fingerprint"].clone();
    assert!(first_fingerprint.is_string());
    assert_eq!(context["declared"]["training_context_tokens"], 8192);
    assert_eq!(context["observed"]["props"]["per_request_tokens"], 768);
    let cache = inputs.server.get("juiced/cache").await;
    let (status,snapshot)=inputs.server.post("juiced/cache/save",json!({"model":MODEL,"slot":0,"session":"native-import-session","expected_generation":cache["generation"],"maximum_bytes":1024})).await;
    #[cfg(target_os = "linux")]
    {
        assert_eq!(status, StatusCode::OK, "{snapshot}");
        assert!(snapshot["engine"]["n_saved"].as_u64().unwrap() > 0);
        assert!(snapshot["engine"]["n_written"].as_u64().unwrap() > 0);
        unload(&inputs.server).await;
        inputs.server.restart().await;
        let (status,restored)=inputs.server.post("juiced/cache/restore",json!({"model":MODEL,"slot":0,"session":"native-import-session","snapshot":snapshot["snapshot"]["id"]})).await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
        assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
        let continued = complete(&inputs.server).await;
        assert_eq!(continued["fixture_restores"], 1);
        unload(&inputs.server).await;
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{snapshot}");
        assert_eq!(snapshot["error"]["code"], "runtime_identity_unsupported");
        assert_eq!(inputs.server.get("juiced/cache").await["data"], json!([]));
        unload(&inputs.server).await;
    }
    let before = inputs.disk();
    let revision = inputs.server.model(MODEL).await["revision"].clone();
    let (status, changed) = inputs
        .server
        .post(
            &format!("models/{MODEL}/options"),
            json!({"expected_revision":revision,"options":{"context_tokens":1024}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["saved"]["context_tokens"], 1024);
    assert_eq!(changed["effective_launch"]["context_tokens"], 1024);
    let edited = inputs.disk();
    assert_eq!(
        edited["native_settings"]["original"],
        imported["native_settings"]["original"]
    );
    assert_ne!(
        edited["native_settings"]["current"],
        before["native_settings"]["current"]
    );
    assert_eq!(
        edited["native_settings"]["current"]["files"]["user_models"],
        before["native_settings"]["current"]["files"]["user_models"]
    );
    let row: Value =
        serde_json::from_slice(&inputs.blob(&edited, false, "recipe_options")).unwrap();
    assert_eq!(row[MODEL]["ctx_size"], 1024);
    assert_eq!(row["opaque-unrelated"]["private"], SOURCE_MARKER);
    let stale = inputs
        .server
        .post(
            &format!("models/{MODEL}/options"),
            json!({"expected_revision":revision,"options":{"context_tokens":2048}}),
        )
        .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["error"]["code"], "revision_conflict");
    assert_eq!(inputs.disk(), edited);
    inputs.server.restart().await;
    assert_eq!(
        inputs.server.get(&format!("models/{MODEL}/options")).await,
        changed
    );
    complete(&inputs.server).await;
    let second_context = inputs
        .server
        .get(&format!("juiced/context?model={MODEL}"))
        .await;
    assert!(second_context["applied"]["execution_fingerprint"].is_string());
    assert_ne!(
        second_context["applied"]["execution_fingerprint"],
        first_fingerprint
    );
    unload(&inputs.server).await;
    let revision = inputs.server.model(MODEL).await["revision"].clone();
    let (status, cleared) = inputs
        .server
        .json(
            Method::POST,
            &format!("/api/v0/models/{MODEL}/options"),
            Some(json!({"expected_revision":revision,"options":{}})),
            inputs.server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(cleared["saved"]["context_tokens"].is_null());
    assert_eq!(cleared["effective_launch"]["context_tokens"], 512);
    inputs.server.restart().await;
    assert_eq!(
        inputs.server.get(&format!("models/{MODEL}/options")).await,
        cleared
    );
    complete(&inputs.server).await;
    assert_eq!(inputs.server.model(MODEL).await["policy"], "on-demand");
    assert_eq!(inputs.source_image(), source);
    let root = inputs.server.directory.path().join(STORE_ROOT);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    for file in std::fs::read_dir(root).unwrap() {
        assert_eq!(
            file.unwrap().metadata().unwrap().permissions().mode() & 0o7777,
            0o600
        );
    }
}

#[tokio::test]
async fn rejected_native_sources_bindings_and_private_store_permissions_never_activate_or_modify_the_source()
 {
    let mut inputs = Inputs::prepare().await;
    inputs.server.stop();
    let original = std::fs::read(&inputs.server.config_path).unwrap();
    let artifact_state = tree(&inputs.server.directory.path().join("artifacts"));
    let recipe = std::fs::read(inputs.source.join("recipe_options.json")).unwrap();
    for failure in [
        "automatic-context",
        "unknown-option",
        "missing-artifact",
        "bad-bundle",
        "mixed-profile",
        "invalid-json",
    ] {
        std::fs::write(inputs.source.join("recipe_options.json"), &recipe).unwrap();
        let mut binding = inputs.binding.clone();
        let code = match failure {
            "automatic-context" => {
                std::fs::write(
                    inputs.source.join("recipe_options.json"),
                    serde_json::to_vec(&json!({MODEL:{"ctx_size":-1}})).unwrap(),
                )
                .unwrap();
                Some("native_context_automatic")
            }
            "unknown-option" => {
                std::fs::write(
                    inputs.source.join("recipe_options.json"),
                    serde_json::to_vec(&json!({MODEL:{(SOURCE_MARKER):"never-echo"}})).unwrap(),
                )
                .unwrap();
                Some("native_execution_unsupported")
            }
            "missing-artifact" => {
                binding["artifact"] = json!("f".repeat(64));
                Some("artifact_not_found")
            }
            "bad-bundle" => {
                binding["execution_profile"]["runtime_bundle"]["files"][0]["sha256"] =
                    json!("0".repeat(64));
                Some("native_artifact_binding")
            }
            "mixed-profile" => {
                binding["execution_profile"]["options"] = json!({"context_tokens":512});
                None
            }
            _ => {
                std::fs::write(
                    inputs.source.join("recipe_options.json"),
                    format!("\"{SOURCE_MARKER}\""),
                )
                .unwrap();
                None
            }
        };
        inputs.write_binding(&binding);
        let source = inputs.source_image();
        rejected(inputs.import().await, code);
        assert_eq!(
            std::fs::read(&inputs.server.config_path).unwrap(),
            original,
            "{failure} activated candidate configuration"
        );
        assert_eq!(
            tree(&inputs.server.directory.path().join("artifacts")),
            artifact_state
        );
        assert_eq!(inputs.source_image(), source);
        inputs.no_pending_generations();
    }
    std::fs::write(inputs.source.join("recipe_options.json"), recipe).unwrap();
    inputs.write_binding(&inputs.binding);
    let private = inputs.server.directory.path().join(STORE_ROOT);
    assert!(private.is_dir());
    std::fs::set_permissions(
        &private,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let source = inputs.source_image();
    rejected(inputs.import().await, None);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&private).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(inputs.source_image(), source);
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), original);
    inputs.no_pending_generations();
    std::fs::set_permissions(private, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    inputs.server.start().await;
    assert_eq!(
        inputs.server.get("juiced/catalog").await["data"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        inputs.server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[cfg(target_os = "linux")]
async fn real_completion(
    server: &Server,
    continuation: bool,
    session: &str,
    slot: Option<u32>,
) -> Value {
    let mut request = json!({"model":MODEL,"prompt":if continuation {"Once upon a time there was"} else {"Once upon a time"},"max_tokens":1,"temperature":0,"stream":false,"cache_prompt":true});
    if let Some(slot) = slot {
        request["id_slot"] = json!(slot);
    }
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", session)
        .json(&request)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "real native completion error: {}",
        body["error"]
    );
    let choices = body["choices"].as_array().expect("completion choices");
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0]["index"], 0);
    assert!(choices[0]["text"].is_string());
    assert!(choices[0]["finish_reason"].is_string());
    let prompt = body["usage"]["prompt_tokens"].as_u64().unwrap();
    assert!(prompt > 0);
    assert_eq!(body["usage"]["completion_tokens"], 1);
    assert_eq!(body["usage"]["total_tokens"], prompt + 1);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let response_model = body["model"].as_str().expect("completion model metadata");
    assert_eq!(
        response_model, MODEL,
        "native response metadata must retain its public model identity"
    );
    json!({"usage":body["usage"],"response_model":response_model,"response_model_matches_request":response_model == MODEL})
}

#[cfg(target_os = "linux")]
async fn real_stream(server: &Server) -> Value {
    let response = server.client.post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .json(&json!({"model":MODEL,"prompt":"Once upon a time","max_tokens":1,"temperature":0,"stream":true}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[reqwest::header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let body = response.text().await.unwrap();
    let mut events = 0;
    let mut done = 0;
    for data in body
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
    {
        if data == "[DONE]" {
            done += 1;
            continue;
        }
        assert_eq!(done, 0, "completion chunks followed the terminal marker");
        let event: Value = serde_json::from_str(data).expect("valid completion SSE data");
        assert_eq!(
            event["model"], MODEL,
            "stream metadata must use the public model identity"
        );
        let choices = event["choices"].as_array().unwrap();
        for choice in choices {
            assert_eq!(choice["index"], 0);
            assert!(choice["text"].is_string());
        }
        events += 1;
    }
    assert!(events > 0);
    assert_eq!(done, 1);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    json!({"events":events,"terminal_markers":done,"public_model":MODEL})
}

#[cfg(target_os = "linux")]
async fn real_observation(inputs: &Inputs, context_tokens: u64) -> Value {
    use std::{collections::BTreeSet, io::Read};
    fn bounded(path: impl AsRef<Path>, maximum: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .unwrap()
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() as u64 <= maximum);
        bytes
    }
    let model = inputs.server.model(MODEL).await;
    assert_eq!(model["loaded"], true);
    assert_eq!(model["runtime_identity"]["state"], "observed");
    assert!(model["runtime_identity"]["bundle_digest"].is_string());
    let pid = model["pid"].as_u64().unwrap();
    let bundle = &inputs.binding["execution_profile"]["runtime_bundle"];
    let root = Path::new(bundle["root"].as_str().unwrap());
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
        root.join("llama-server")
    );
    let argv = bounded(format!("/proc/{pid}/cmdline"), 65536);
    let argv: Vec<_> = argv
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect();
    let context = context_tokens.to_string();
    for (flag, value) in [
        ("--ctx-size", context.as_str()),
        ("--gpu-layers", "0"),
        ("--parallel", "1"),
        ("--host", "127.0.0.1"),
        ("--alias", MODEL),
    ] {
        assert_eq!(
            argv.iter().filter(|arg| **arg == flag.as_bytes()).count(),
            1
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == [flag.as_bytes(), value.as_bytes()])
        );
    }
    for flag in ["--jinja", "--metrics"] {
        assert_eq!(
            argv.iter().filter(|arg| **arg == flag.as_bytes()).count(),
            1
        );
    }
    let context = inputs
        .server
        .get(&format!("juiced/context?model={MODEL}"))
        .await;
    assert_eq!(
        context["applied"]["launch"]["context_tokens"],
        context_tokens
    );
    let training = context["declared"]["training_context_tokens"]
        .as_u64()
        .expect("declared training context");
    assert!(training > 0);
    let observed_context = context_tokens.min(training);
    assert_eq!(
        context["observed"]["props"]["per_request_tokens"], observed_context,
        "selected engine context observation: {context}"
    );
    assert_eq!(context["observed"]["slots"].as_array().unwrap().len(), 1);
    assert_eq!(
        context["observed"]["slots"][0]["context_tokens"],
        observed_context
    );
    assert!(context["observation_error"].is_null());
    let maps = bounded(format!("/proc/{pid}/maps"), 1024 * 1024);
    let maps = std::str::from_utf8(&maps).unwrap();
    let mut paths = BTreeSet::new();
    for line in maps.lines() {
        if !line.split_whitespace().nth(1).unwrap().contains('x') {
            continue;
        }
        let mut tail = line;
        for _ in 0..5 {
            tail = tail.trim_start();
            tail = &tail[tail.find(char::is_whitespace).unwrap_or(tail.len())..];
        }
        let path = tail.trim_start();
        if path.starts_with('/') {
            assert!(!path.ends_with(" (deleted)"));
            paths.insert(PathBuf::from(path));
        }
    }
    assert!(!paths.is_empty() && paths.len() <= 256);
    let mut bytes = 0;
    let modules: Vec<_> = paths.into_iter().map(|path| {
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.is_file() && metadata.len() <= 256 * 1024 * 1024);
        bytes += metadata.len();
        assert!(bytes <= 1024 * 1024 * 1024);
        json!({"name":path.file_name().unwrap().to_str().unwrap(),"sha256":digest(&path),"bytes":metadata.len(),"in_bundle":path.starts_with(root)})
    }).collect();
    assert!(modules.iter().any(|module| module["in_bundle"] == true));
    for module in &modules {
        let name = module["name"].as_str().unwrap();
        if name.starts_with("libllama")
            || name.starts_with("libggml")
            || name.starts_with("libmtmd")
        {
            assert_eq!(
                module["in_bundle"], true,
                "selected engine companion escaped the exact bundle: {name}"
            );
        }
    }
    json!({"pid":pid,"runtime_identity":model["runtime_identity"],"process_lifecycle":model["process_lifecycle"],"context_tokens":context_tokens,"training_context_tokens":training,"observed_context_tokens":observed_context,"effective_argument_checks":["--ctx-size","--gpu-layers 0","--parallel 1","--host 127.0.0.1","--jinja","--metrics"],"mapped_modules":modules})
}

#[cfg(target_os = "linux")]
async fn real_unload(inputs: &Inputs, observation: &Value) {
    unload(&inputs.server).await;
    let pid = observation["pid"].as_u64().unwrap();
    inputs
        .server
        .wait_for(async || !Path::new(&format!("/proc/{pid}")).exists())
        .await;
    assert_eq!(
        inputs.server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; qualifies selected Linux CPU native import and slot pipeline"]
async fn tiny_real_native_import_load_cache_restart_edit_and_clear_pipeline() {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let metadata = std::fs::metadata(&model).unwrap();
    assert!(
        metadata.is_file() && metadata.len() <= 64 * 1024 * 1024,
        "selected tiny GGUF exceeds64MiB test bound"
    );
    let model_digest = digest(&model);
    let weights = std::fs::read(&model).unwrap();
    assert_eq!(digest(&model), model_digest);
    let mut inputs = Inputs::prepare_from(Some(&engine), weights).await;
    assert_eq!(inputs.artifact, model_digest);
    let source = inputs.source_image();
    let bundle = inputs.binding["execution_profile"]["runtime_bundle"].clone();
    let bundle_image = tree(Path::new(bundle["root"].as_str().unwrap()));
    inputs.server.stop();
    let imported = success(inputs.import().await);
    assert_eq!(imported["models_added"], 1);
    assert_eq!(imported["source_modified"], false);
    let original = inputs.disk()["native_settings"]["original"].clone();
    inputs.server.start().await;
    let disabled = inputs.server.model(MODEL).await;
    assert_eq!(disabled["policy"], "disabled");
    assert!(disabled["pid"].is_null() && disabled["process_lifecycle"].is_null());
    assert_eq!(
        inputs.server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    let (status, enabled) = inputs
        .server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"on-demand","expected_revision":disabled["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let first_usage =
        real_completion(&inputs.server, false, "native-real-import-session", Some(0)).await;
    let first = real_observation(&inputs, 768).await;
    let cache = inputs.server.get("juiced/cache").await;
    let (status, saved) = inputs.server.post("juiced/cache/save", json!({"model":MODEL,"slot":0,"session":"native-real-import-session","expected_generation":cache["generation"],"maximum_bytes":16*1024*1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    real_unload(&inputs, &first).await;
    inputs.server.restart().await;
    let (status, restored) = inputs.server.post("juiced/cache/restore", json!({"model":MODEL,"slot":0,"session":"native-real-import-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    let continued_usage =
        real_completion(&inputs.server, true, "native-real-import-session", Some(0)).await;
    let resumed = real_observation(&inputs, 768).await;
    assert_ne!(resumed["pid"], first["pid"]);
    assert_eq!(
        resumed["runtime_identity"]["closure_digest"],
        first["runtime_identity"]["closure_digest"]
    );
    real_unload(&inputs, &resumed).await;
    let mut edits = Vec::new();
    for context in [Some(1024), None] {
        let revision = inputs.server.model(MODEL).await["revision"].clone();
        let options =
            context.map_or_else(|| json!({}), |context| json!({"context_tokens":context}));
        let (status, changed) = inputs
            .server
            .post(
                &format!("models/{MODEL}/options"),
                json!({"expected_revision":revision,"options":options}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(changed["saved"]["context_tokens"], json!(context));
        assert_eq!(
            changed["effective_launch"]["context_tokens"],
            context.unwrap_or(512)
        );
        inputs.server.restart().await;
        assert_eq!(
            inputs.server.get(&format!("models/{MODEL}/options")).await,
            changed
        );
        let usage =
            real_completion(&inputs.server, false, "native-real-import-session", Some(0)).await;
        let observation = real_observation(&inputs, context.unwrap_or(512)).await;
        real_unload(&inputs, &observation).await;
        edits.push(json!({"saved_context_tokens":context,"completion":usage,"observation":observation,"exit_acknowledged":true}));
    }
    let streaming = real_stream(&inputs.server).await;
    let streaming_observation = real_observation(&inputs, 512).await;
    real_unload(&inputs, &streaming_observation).await;
    assert_eq!(inputs.disk()["native_settings"]["original"], original);
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(Path::new(bundle["root"].as_str().unwrap())),
        bundle_image
    );
    let manager = inputs.server.pid();
    inputs.server.stop();
    assert!(!Path::new(&format!("/proc/{manager}")).exists());
    assert_eq!(
        digest(&engine),
        bundle["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "llama-server")
            .unwrap()["sha256"]
    );
    assert_eq!(digest(&model), model_digest);
    eprintln!(
        "native-real-import-receipt {}",
        json!({"schema_version":1,"scope":"selected Linux CPU native metadata import, exact bundle and managed GGUF, one-token structural completion, slot persistence, native context edits; no output quality or benchmark qualification","model":{"sha256":model_digest,"bytes":metadata.len()},"bundle":{"entrypoint":bundle["entrypoint"],"files":bundle["files"]},"first":{"completion":first_usage,"observation":first,"exit_acknowledged":true},"resumed":{"completion":continued_usage,"observation":resumed,"exit_acknowledged":true},"cache":{"saved":saved["engine"],"restored":restored["engine"]},"edits":edits,"streaming":{"framing":streaming,"observation":streaming_observation,"exit_acknowledged":true},"source_unchanged":true,"bundle_unchanged":true,"owned_manager_exit_acknowledged":true})
    );
}

#[cfg(target_os = "linux")]
mod protected_state {
    use super::*;
    use tokio::io::AsyncWriteExt;

    const SESSION: &str = "protected-state-session";
    const STATE_BYTES: u64 = 4 * 1024 * 1024;

    struct FixtureControl {
        base: String,
        pid: u64,
    }
    impl FixtureControl {
        async fn owned(server: &Server) -> Self {
            use std::io::Read;
            let pid = server.model(MODEL).await["pid"].as_u64().unwrap();
            let mut argv = Vec::new();
            std::fs::File::open(format!("/proc/{pid}/cmdline"))
                .unwrap()
                .take(65537)
                .read_to_end(&mut argv)
                .unwrap();
            assert!(argv.len() <= 65536);
            let argv: Vec<_> = argv
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .collect();
            let port: u16 =
                std::str::from_utf8(argv.windows(2).find(|pair| pair[0] == b"--port").unwrap()[1])
                    .unwrap()
                    .parse()
                    .unwrap();
            let control = Self {
                base: format!("http://127.0.0.1:{port}"),
                pid,
            };
            control.observe(server).await;
            control
        }
        async fn observe(&self, server: &Server) -> Value {
            let response = server
                .client
                .get(format!("{}/__fixture/state", self.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value = response.json().await.unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["pid"], self.pid);
            value
        }
        async fn arm(
            &self,
            server: &Server,
            operation: &str,
            effect: bool,
            eof: bool,
            reply: &str,
            status: u16,
        ) -> u64 {
            let response=server.client.post(format!("{}/__fixture/state/control",self.base)).json(&json!({"action":"arm","operation":operation,"hold_effect":effect,"hold_eof":eof,"reply":reply,"status":status})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.json::<Value>().await.unwrap()["operation_id"]
                .as_u64()
                .unwrap()
        }
        async fn release(&self, server: &Server, id: u64, phase: &str) {
            let response = server
                .client
                .post(format!("{}/__fixture/state/control", self.base))
                .json(&json!({"action":phase,"operation_id":id}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        async fn until(
            &self,
            server: &Server,
            id: u64,
            condition: impl Fn(&Value) -> bool,
        ) -> Value {
            let settled = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let view = self.observe(server).await;
                    let current = &view["jobs"]["current"];
                    if current["id"] == id && condition(current) {
                        return current.clone();
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            assert!(
                settled.is_ok(),
                "fixture operation{id} did not reach expected phase: {}",
                self.observe(server).await
            );
            settled.unwrap()
        }
    }
    async fn prime(server: &Server) -> Value {
        let response = server
            .client
            .post(format!("{}/v1/completions", server.base))
            .bearer_auth(server.admin_key().unwrap())
            .header("x-juiced-session", SESSION)
            .json(&json!({"model":MODEL,"prompt":"protected state data pipeline","max_tokens":4}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        host_claims(server, HOST_RESIDENT, 0, 0).await;
        body
    }
    async fn inputs() -> (Inputs, FixtureControl) {
        assert_eq!(
            std::env::var("JUICED_TEST_HOST_MEMORY").as_deref(),
            Ok("1"),
            "set JUICED_TEST_HOST_MEMORY=1"
        );
        let (mut inputs, fingerprint) = protected_baseline().await;
        let mut config = protected_config(&inputs, &fingerprint, Some(256 * 1024 * 1024), 5000);
        config["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["id"] == MODEL)
            .unwrap()["resource_profile"]["state_bytes"] = json!(STATE_BYTES);
        write_config(&inputs, &config);
        inputs.server.start().await;
        let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
        prime(&inputs.server).await;
        let model = inputs.server.model(MODEL).await;
        let (status, policy) = inputs
            .server
            .post(
                "juiced/policy",
                json!({"model":MODEL,"policy":"hot","expected_revision":model["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        let control = FixtureControl::owned(&inputs.server).await;
        (inputs, control)
    }
    async fn save_body(server: &Server) -> Value {
        let cache = server.get("juiced/cache").await;
        json!({"model":MODEL,"slot":0,"session":SESSION,"expected_generation":cache["generation"],"maximum_bytes":1024})
    }
    fn start(
        server: &Server,
        path: &str,
        body: Value,
    ) -> tokio::task::JoinHandle<(StatusCode, Value)> {
        let request = server
            .client
            .post(format!("{}/v1/{path}", server.base))
            .bearer_auth(server.admin_key().unwrap())
            .json(&body);
        tokio::spawn(async move {
            let response = request.send().await.unwrap();
            let status = response.status();
            (status, response.json().await.unwrap())
        })
    }
    async fn socket(server: &Server, path: &str, body: Value) -> tokio::net::TcpStream {
        let mut socket = tokio::net::TcpStream::connect(server.base.trim_start_matches("http://"))
            .await
            .unwrap();
        let bytes = serde_json::to_vec(&body).unwrap();
        let headers = format!(
            "POST /v1/{path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            server.admin_key().unwrap(),
            bytes.len()
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(&bytes).await.unwrap();
        socket
    }
    async fn metadata(server: &Server, id: &str) -> Value {
        server.get("juiced/cache").await["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap()
            .clone()
    }
    async fn pending(server: &Server, except: Option<&str>) -> Value {
        let rows = server.get("juiced/cache").await;
        let rows: Vec<_> = rows["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| Some(row["id"].as_str().unwrap()) != except && row["state"] != "saved")
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["state"], "pending");
        assert_eq!(rows[0]["writer_bound"], true);
        assert_eq!(rows[0]["writer_current_owner"], true);
        assert!(rows[0]["writer_actor_present"].is_boolean());
        assert_eq!(rows[0]["reserved_bytes"], 1024);
        rows[0].clone()
    }
    async fn retained(inputs: &Inputs, pid: u64) {
        inputs
            .server
            .wait_for(async || inputs.server.model(MODEL).await["uncertain_state_operations"] == 1)
            .await;
        let claims = host_claims(&inputs.server, HOST_RESIDENT, STATE_BYTES, 0).await;
        let model = inputs.server.model(MODEL).await;
        assert_eq!(model["pid"], pid);
        assert_eq!(model["policy"], "hot");
        assert_eq!(model["active_requests"], 1);
        assert_eq!(model["uncertain_requests"], 0);
        assert_eq!(model["error"], "state_termination_unconfirmed");
        assert_eq!(model["cleanup_retry_available"], true);
        assert_eq!(claims["resources"]["power"]["active"], 1);
        assert_eq!(claims["scheduler"]["active"], 1);
        assert!(Path::new(&format!("/proc/{pid}")).exists());
    }
    async fn exit(inputs: &Inputs, pid: u64) {
        let (status, unloaded) = inputs.server.post("unload", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{unloaded}");
        assert_eq!(unloaded["policy"], "hot");
        no_protected_child(&inputs.server).await;
        assert_eq!(
            inputs.server.model(MODEL).await["uncertain_state_operations"],
            0
        );
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    async fn ordinary_cleanup(inputs: &Inputs) {
        let model = inputs.server.model(MODEL).await;
        let (status, policy) = inputs
            .server
            .post(
                "juiced/policy",
                json!({"model":MODEL,"policy":"on-demand","expected_revision":model["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        if model["loaded"] == true {
            unload(&inputs.server).await;
        }
        no_protected_child(&inputs.server).await;
    }

    async fn recover(server: &Server, row: &Value) {
        let (status, recovered) = server
            .post(
                "juiced/cache/recover",
                json!({"snapshot":row["id"],"session":SESSION,"expected_revision":row["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{recovered}");
        assert_eq!(recovered["recovered"], true);
        assert_eq!(recovered["journal_cleanup_pending"], false);
    }

    fn retained_reader(row: &Value) -> Value {
        assert_eq!(row["state"], "saved");
        let readers = row["retained_readers"].as_array().unwrap();
        assert_eq!(readers.len(), 1, "{row}");
        let reader = &readers[0];
        let id = reader["id"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert!(
            reader["revision"]
                .as_u64()
                .is_some_and(|revision| revision > 0)
        );
        assert_eq!(reader["writer_current_owner"], true);
        assert!(reader["writer_actor_present"].is_boolean());
        assert_eq!(reader["release_eligible"], false);
        for private in [
            "pid",
            "actor",
            "manager_owner",
            "write_key",
            "binding_digest",
        ] {
            assert!(reader.get(private).is_none());
        }
        reader.clone()
    }

    async fn recover_reader(server: &Server, snapshot: &str, reader: &Value) {
        let(status,recovered)=server.post("juiced/cache/recover",json!({"snapshot":snapshot,"session":SESSION,"reader":reader["id"],"expected_revision":reader["revision"]})).await;
        assert_eq!(status, StatusCode::OK, "{recovered}");
        assert_eq!(recovered["recovered"], true);
        assert_eq!(recovered["snapshot"], snapshot);
        assert_eq!(recovered["reader"], reader["id"]);
        assert_eq!(recovered["journal_cleanup_pending"], false);
    }

    async fn queued_reader_barrier(
        inputs: &Inputs,
        control: &FixtureControl,
        snapshot: &Value,
        path: &Path,
        checksum: &str,
    ) -> FixtureControl {
        let stats = control.observe(&inputs.server).await;
        let mut holder=inputs.server.client.post(format!("{}/v1/completions",inputs.server.base)).bearer_auth(inputs.server.admin_key().unwrap()).header("x-juiced-session",SESSION).json(&json!({"model":MODEL,"prompt":"hold the slot before reader binding","max_tokens":4,"stream":true})).send().await.unwrap();
        assert_eq!(holder.status(), StatusCode::OK);
        assert!(!holder.chunk().await.unwrap().unwrap().is_empty());
        let id = control
            .arm(&inputs.server, "restore", false, true, "malformed", 200)
            .await;
        let restore = start(
            &inputs.server,
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":snapshot["id"]}),
        );
        inputs
            .server
            .wait_for(async || {
                inputs.server.get("juiced/admission").await["scheduler"]["pending"] == 1
            })
            .await;
        let queued = start(
            &inputs.server,
            "juiced/cache/save",
            save_body(&inputs.server).await,
        );
        inputs
            .server
            .wait_for(async || {
                inputs.server.get("juiced/admission").await["scheduler"]["pending"] == 2
            })
            .await;
        assert_eq!(
            metadata(&inputs.server, snapshot["id"].as_str().unwrap()).await["retained_readers"],
            json!([])
        );
        holder.bytes().await.unwrap();
        control
            .until(&inputs.server, id, |job| {
                job["reply_started"] == true && job["physical_settled"] == true
            })
            .await;
        let reading = metadata(&inputs.server, snapshot["id"].as_str().unwrap()).await;
        assert_eq!(reading["active_readers"], 1);
        let reader = retained_reader(&reading);
        control.release(&inputs.server, id, "release-eof").await;
        let (status, error) = restore.await.unwrap();
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
        assert_eq!(error["error"]["code"], "cache_acknowledgement_uncertain");
        let (status, error) = queued.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
        host_claims(&inputs.server, HOST_RESIDENT, 0, 0).await;
        assert_eq!(
            control.observe(&inputs.server).await["save_started"],
            stats["save_started"]
        );
        assert_eq!(inputs.server.model(MODEL).await["pid"], control.pid);
        assert_eq!(
            inputs.server.model(MODEL).await["uncertain_state_operations"],
            0
        );
        let retained = metadata(&inputs.server, snapshot["id"].as_str().unwrap()).await;
        assert_eq!(retained["active_readers"], 0);
        assert_eq!(retained["retained_readers"], json!([reader]));
        assert_eq!(digest(path), checksum);
        ordinary_cleanup(inputs).await;
        assert!(!Path::new(&format!("/proc/{}", control.pid)).exists());
        recover_reader(&inputs.server, snapshot["id"].as_str().unwrap(), &reader).await;
        assert_eq!(
            metadata(&inputs.server, snapshot["id"].as_str().unwrap()).await,
            *snapshot
        );
        assert_eq!(digest(path), checksum);
        let model = inputs.server.model(MODEL).await;
        let (status, policy) = inputs
            .server
            .post(
                "juiced/policy",
                json!({"model":MODEL,"policy":"hot","expected_revision":model["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
        prime(&inputs.server).await;
        FixtureControl::owned(&inputs.server).await
    }

    #[tokio::test]
    async fn explicit_state_ceiling_with_unknown_capacity_creates_no_writer_or_staging_file() {
        let (mut inputs, fingerprint) = protected_baseline().await;
        let mut config = protected_config(&inputs, &fingerprint, None, 5000);
        config["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["id"] == MODEL)
            .unwrap()["resource_profile"]["state_bytes"] = json!(STATE_BYTES);
        write_config(&inputs, &config);
        let manifest = inputs
            .server
            .directory
            .path()
            .join("kv-cache/snapshots.json");
        std::fs::write(&manifest, b"UNOPENED_CACHE_METADATA_MUST_NOT_BE_PARSED").unwrap();
        inputs.server.start().await;
        let (cache_status, cache) = inputs
            .server
            .json(
                Method::GET,
                "/v1/juiced/cache",
                None,
                inputs.server.admin_key(),
            )
            .await;
        assert_eq!(cache_status, StatusCode::SERVICE_UNAVAILABLE, "{cache}");
        assert_eq!(cache["error"]["code"], "unknown_host_capacity");
        let files = tree(&inputs.server.directory.path().join("kv-cache"));
        let source = inputs.source_image();
        let bytes = std::fs::read(&inputs.server.config_path).unwrap();
        let (status, error) = inputs
            .server
            .post("juiced/cache/save", json!({"model":MODEL,"slot":0,"session":SESSION,"expected_generation":0,"maximum_bytes":1024}))
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
        assert_eq!(error["error"]["code"], "unknown_host_capacity");
        no_protected_child(&inputs.server).await;
        let (status, after) = inputs
            .server
            .json(
                Method::GET,
                "/v1/juiced/cache",
                None,
                inputs.server.admin_key(),
            )
            .await;
        assert_eq!(status, cache_status);
        assert_eq!(after, cache);
        assert_eq!(
            tree(&inputs.server.directory.path().join("kv-cache")),
            files
        );
        assert_eq!(inputs.source_image(), source);
        assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), bytes);
    }

    #[tokio::test]
    #[ignore = "requires JUICED_TEST_HOST_MEMORY=1; selected protected state EOF and shared-slot lifecycle"]
    async fn original_eof_controls_publication_and_shared_state_slot_queues_cancel_without_dispatch()
     {
        let (inputs, control) = inputs().await;
        let source = inputs.source_image();
        let config = std::fs::read(&inputs.server.config_path).unwrap();
        let stats = control.observe(&inputs.server).await;
        let id = control
            .arm(&inputs.server, "save", true, true, "valid", 200)
            .await;
        let save = start(
            &inputs.server,
            "juiced/cache/save",
            save_body(&inputs.server).await,
        );
        control
            .until(&inputs.server, id, |job| {
                job["physical_started"] == true && job["effect_started"] == false
            })
            .await;
        host_claims(&inputs.server, HOST_RESIDENT, STATE_BYTES, 0).await;
        let first_pending = pending(&inputs.server, None).await;
        let queued_state = socket(
            &inputs.server,
            "juiced/cache/save",
            save_body(&inputs.server).await,
        )
        .await;
        inputs
            .server
            .wait_for(async || {
                inputs.server.get("juiced/admission").await["scheduler"]["pending"] == 1
            })
            .await;
        assert_eq!(
            control.observe(&inputs.server).await["save_started"]
                .as_u64()
                .unwrap(),
            stats["save_started"].as_u64().unwrap() + 1
        );
        drop(queued_state);
        inputs
            .server
            .wait_for(async || {
                inputs.server.get("juiced/admission").await["scheduler"]["pending"] == 0
            })
            .await;
        assert_eq!(
            inputs.server.get("juiced/cache").await["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let queued_body = json!({"model":MODEL,"prompt":"queued behind state work","max_tokens":4});
        let ingress = queued_body.to_string().len() as u64 * 64;
        let queued_inference = inputs
            .server
            .socket_request("completions", queued_body)
            .await;
        inputs
            .server
            .wait_for(async || {
                inputs.server.get("juiced/admission").await["scheduler"]["pending"] == 1
            })
            .await;
        let queued = inputs.server.get("juiced/admission").await;
        assert_eq!(
            queued["scheduler"]["memory"]["reserved_bytes"],
            HOST_RESIDENT + STATE_BYTES + ingress + cache_control_claims(&queued).2
        );
        assert_eq!(
            control.observe(&inputs.server).await["inference_started"],
            stats["inference_started"]
        );
        let cache = inputs.server.get("juiced/cache").await;
        let (status, error) = inputs
            .server
            .post(
                "juiced/cache",
                json!({"enabled":false,"expected_generation":cache["configuration_generation"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "cache_busy");
        drop(queued_inference);
        inputs
            .server
            .wait_for(async || {
                inputs.server.get("juiced/admission").await["scheduler"]["pending"] == 0
            })
            .await;
        host_claims(&inputs.server, HOST_RESIDENT, STATE_BYTES, 0).await;
        assert_eq!(
            control.observe(&inputs.server).await["inference_started"],
            stats["inference_started"]
        );
        control.release(&inputs.server, id, "release-effect").await;
        let written = control
            .until(&inputs.server, id, |job| {
                job["physical_settled"] == true && job["reply_started"] == true
            })
            .await;
        assert_eq!(written["effect_completed"], true);
        assert_eq!(written["reply_eof"], false);
        assert!(written["bytes"].as_u64().unwrap() > 0);
        assert!(
            !save.is_finished(),
            "a JSON acknowledgement without original EOF published a snapshot"
        );
        assert_eq!(
            metadata(&inputs.server, first_pending["id"].as_str().unwrap()).await["state"],
            "pending"
        );
        host_claims(&inputs.server, HOST_RESIDENT, STATE_BYTES, 0).await;
        control.release(&inputs.server, id, "release-eof").await;
        let (status, saved) = save.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
        assert_eq!(saved["engine"]["n_written"], written["bytes"]);
        let snapshot = saved["snapshot"].clone();
        assert_eq!(snapshot["active_readers"], 0);
        assert_eq!(snapshot["retained_readers"], json!([]));
        host_claims(&inputs.server, HOST_RESIDENT, 0, 0).await;
        let mut uncertain = Vec::new();
        for (reply, status, code) in [
            ("malformed", 200, "cache_acknowledgement_uncertain"),
            ("valid", 500, "cache_save_rejected"),
        ] {
            let id = control
                .arm(&inputs.server, "save", false, false, reply, status)
                .await;
            let (status, error) = start(
                &inputs.server,
                "juiced/cache/save",
                save_body(&inputs.server).await,
            )
            .await
            .unwrap();
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
            assert_eq!(error["error"]["code"], code);
            control
                .until(&inputs.server, id, |job| {
                    job["reply_eof"] == true && job["physical_settled"] == true
                })
                .await;
            host_claims(&inputs.server, HOST_RESIDENT, 0, 0).await;
            let model = inputs.server.model(MODEL).await;
            assert_eq!(model["pid"], control.pid);
            assert_eq!(model["uncertain_state_operations"], 0);
            let cache = inputs.server.get("juiced/cache").await;
            let row = cache["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| {
                    row["id"] != snapshot["id"]
                        && !uncertain
                            .iter()
                            .any(|previous: &Value| previous["id"] == row["id"])
                })
                .unwrap()
                .clone();
            assert_eq!(row["state"], "uncertain");
            assert_eq!(row["writer_bound"], true);
            let(status,error)=inputs.server.post("juiced/cache/recover",json!({"snapshot":row["id"],"session":SESSION,"expected_revision":row["revision"]})).await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["error"]["code"], "snapshot_uncertain");
            uncertain.push(row);
        }
        let inspect = inputs.server.get("juiced/cache/inspect").await;
        assert_eq!(
            inspect["charged_bytes"],
            snapshot["bytes"].as_u64().unwrap() + 2048
        );
        assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), config);
        ordinary_cleanup(&inputs).await;
        assert!(!Path::new(&format!("/proc/{}", control.pid)).exists());
        for row in &uncertain {
            recover(&inputs.server, row).await;
        }
        assert_eq!(
            inputs.server.get("juiced/cache").await["data"],
            json!([snapshot])
        );
        assert_eq!(inputs.source_image(), source);
    }

    #[tokio::test]
    #[ignore = "requires JUICED_TEST_HOST_MEMORY=1; selected protected file actor and reader lifetime"]
    async fn cancelled_file_work_and_snapshot_reader_remain_owned_until_exact_generation_exit() {
        let (inputs, control) = inputs().await;
        let source = inputs.source_image();
        let (status, saved) = start(
            &inputs.server,
            "juiced/cache/save",
            save_body(&inputs.server).await,
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::OK, "{saved}");
        let snapshot = saved["snapshot"].clone();
        let id = snapshot["id"].as_str().unwrap();
        let path = inputs
            .server
            .directory
            .path()
            .join("kv-cache")
            .join(format!("kv-{id}.bin"));
        let checksum = digest(&path);
        let restore_id = control
            .arm(&inputs.server, "restore", true, false, "valid", 200)
            .await;
        let request = socket(
            &inputs.server,
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":id}),
        )
        .await;
        control
            .until(&inputs.server, restore_id, |job| {
                job["physical_started"] == true && job["effect_started"] == false
            })
            .await;
        host_claims(&inputs.server, HOST_RESIDENT, STATE_BYTES, 0).await;
        let reading = metadata(&inputs.server, id).await;
        assert_eq!(reading["active_readers"], 1);
        let reader = retained_reader(&reading);
        drop(request);
        control
            .until(&inputs.server, restore_id, |job| {
                job["transport_dropped"] == true && job["physical_settled"] == false
            })
            .await;
        retained(&inputs, control.pid).await;
        let (status, error) = inputs
            .server
            .post(
                "juiced/cache/forget",
                json!({"snapshot":id,"session":SESSION,"expected_revision":reading["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
        let(status,error)=inputs.server.post("juiced/cache/recover",json!({"snapshot":id,"session":SESSION,"reader":reader["id"],"expected_revision":reader["revision"]})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
        control
            .release(&inputs.server, restore_id, "release-effect")
            .await;
        let physical = control
            .until(&inputs.server, restore_id, |job| {
                job["physical_settled"] == true
            })
            .await;
        assert_eq!(physical["effect_completed"], true);
        assert_eq!(physical["bytes"], snapshot["bytes"]);
        assert_eq!(physical["reply_eof"], false);
        retained(&inputs, control.pid).await;
        let still_reading = metadata(&inputs.server, id).await;
        assert_eq!(still_reading["active_readers"], 1);
        assert_eq!(still_reading["retained_readers"], json!([reader]));
        assert_eq!(digest(&path), checksum);
        exit(&inputs, control.pid).await;
        let exited = metadata(&inputs.server, id).await;
        assert_eq!(exited["active_readers"], 0);
        assert_eq!(exited["retained_readers"], json!([reader]));
        let (status, error) = inputs
            .server
            .post(
                "juiced/cache/forget",
                json!({"snapshot":id,"session":SESSION,"expected_revision":exited["revision"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
        for (session, revision, code) in [
            (
                SESSION,
                reader["revision"].as_u64().unwrap() + 1,
                "cache_revision_conflict",
            ),
            (
                "wrong-reader-session",
                reader["revision"].as_u64().unwrap(),
                "snapshot_identity_mismatch",
            ),
        ] {
            let(status,error)=inputs.server.post("juiced/cache/recover",json!({"snapshot":id,"session":session,"reader":reader["id"],"expected_revision":revision})).await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["error"]["code"], code);
            assert_eq!(metadata(&inputs.server, id).await, exited);
            assert_eq!(digest(&path), checksum);
        }
        let (status, error) = inputs
            .server
            .post(
                "juiced/cache/restore",
                json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":id}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
        no_protected_child(&inputs.server).await;
        recover_reader(&inputs.server, id, &reader).await;
        assert_eq!(metadata(&inputs.server, id).await, snapshot);
        assert_eq!(digest(&path), checksum);
        let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
        let next = FixtureControl::owned(&inputs.server).await;
        assert_ne!(next.pid, control.pid);
        let (status, restored) = inputs
            .server
            .post(
                "juiced/cache/restore",
                json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":id}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
        assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
        assert_eq!(restored["retained_reader_cleanup_pending"], false);
        assert_eq!(restored["snapshot"]["retained_readers"], json!([]));
        let completed = metadata(&inputs.server, id).await;
        assert_eq!(completed["active_readers"], 0);
        assert_eq!(completed["retained_readers"], json!([]));
        assert_eq!(prime(&inputs.server).await["fixture_restores"], 1);
        let next = queued_reader_barrier(&inputs, &next, &snapshot, &path, &checksum).await;
        let save_id = next
            .arm(&inputs.server, "save", true, false, "valid", 200)
            .await;
        let request = socket(
            &inputs.server,
            "juiced/cache/save",
            save_body(&inputs.server).await,
        )
        .await;
        next.until(&inputs.server, save_id, |job| {
            job["physical_started"] == true && job["effect_started"] == false
        })
        .await;
        let pending = pending(&inputs.server, Some(id)).await;
        let staging = inputs
            .server
            .directory
            .path()
            .join("kv-cache")
            .join(format!("kv-{}.bin", pending["id"].as_str().unwrap()));
        assert_eq!(std::fs::metadata(&staging).unwrap().len(), 0);
        drop(request);
        next.until(&inputs.server, save_id, |job| {
            job["transport_dropped"] == true && job["physical_settled"] == false
        })
        .await;
        retained(&inputs, next.pid).await;
        let(status,error)=inputs.server.post("juiced/cache/recover",json!({"snapshot":pending["id"],"session":SESSION,"expected_revision":pending["revision"]})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "snapshot_busy");
        next.release(&inputs.server, save_id, "release-effect")
            .await;
        let physical = next
            .until(&inputs.server, save_id, |job| {
                job["physical_settled"] == true
            })
            .await;
        assert_eq!(physical["effect_completed"], true);
        assert_eq!(physical["reply_eof"], false);
        assert!(physical["bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            std::fs::metadata(&staging).unwrap().len(),
            physical["bytes"].as_u64().unwrap()
        );
        retained(&inputs, next.pid).await;
        assert_eq!(
            metadata(&inputs.server, pending["id"].as_str().unwrap()).await["state"],
            "pending"
        );
        assert_eq!(digest(&path), checksum);
        exit(&inputs, next.pid).await;
        recover(&inputs.server, &pending).await;
        assert!(!staging.exists());
        assert_eq!(metadata(&inputs.server, id).await["state"], "saved");
        assert_eq!(digest(&path), checksum);
        assert_eq!(inputs.source_image(), source);
    }

    #[tokio::test]
    #[ignore = "requires JUICED_TEST_HOST_MEMORY=1; selected protected state inspection lifecycle"]
    async fn interrupted_inspection_retains_state_without_creating_a_save_intent() {
        let (inputs, control) = inputs().await;
        let cache = inputs.server.get("juiced/cache").await;
        assert_eq!(cache["data"], json!([]));
        let tree_before = tree(&inputs.server.directory.path().join("kv-cache"));
        let stats = control.observe(&inputs.server).await;
        let id = control
            .arm(
                &inputs.server,
                "inspect-slots",
                false,
                false,
                "interrupted",
                200,
            )
            .await;
        let (status, error) = start(
            &inputs.server,
            "juiced/cache/save",
            save_body(&inputs.server).await,
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
        assert_eq!(error["error"]["code"], "state_transport_uncertain");
        control
            .until(&inputs.server, id, |job| {
                job["physical_settled"] == true && job["transport_dropped"] == true
            })
            .await;
        retained(&inputs, control.pid).await;
        assert_eq!(inputs.server.get("juiced/cache").await, cache);
        assert_eq!(
            tree(&inputs.server.directory.path().join("kv-cache")),
            tree_before
        );
        assert_eq!(
            control.observe(&inputs.server).await["save_started"],
            stats["save_started"]
        );
        exit(&inputs, control.pid).await;
        assert_eq!(inputs.server.get("juiced/cache").await, cache);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_HOST_MEMORY=1, JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; selected protected CPU state pipeline"]
async fn tiny_real_protected_state_save_restart_restore_and_continue_pipeline() {
    assert_eq!(
        std::env::var("JUICED_TEST_HOST_MEMORY").as_deref(),
        Ok("1"),
        "set JUICED_TEST_HOST_MEMORY=1"
    );
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let model_metadata = std::fs::metadata(&model).unwrap();
    assert!(model_metadata.is_file() && model_metadata.len() <= 64 * 1024 * 1024);
    let model_sha = digest(&model);
    let engine_sha = digest(&engine);
    let weights = std::fs::read(&model).unwrap();
    let (mut inputs, fingerprint) =
        protected_baseline_with(Inputs::prepare_from(Some(&engine), weights).await).await;
    let source = inputs.source_image();
    let bundle = inputs.binding["execution_profile"]["runtime_bundle"].clone();
    let bundle_image = tree(Path::new(bundle["root"].as_str().unwrap()));
    let mut config = protected_config(&inputs, &fingerprint, Some(1024 * 1024 * 1024), 30000);
    config["request_timeout_ms"] = json!(30000);
    config["startup_timeout_ms"] = json!(30000);
    config["admission"]["memory"]["essential_headroom_bytes"] = json!(64 * 1024 * 1024);
    let profile = &mut config["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["id"] == MODEL)
        .unwrap()["resource_profile"];
    profile["load_bytes"] = json!(512 * 1024 * 1024);
    profile["resident_bytes"] = json!(256 * 1024 * 1024);
    profile["request_bytes"] = json!(64 * 1024 * 1024);
    profile["state_bytes"] = json!(128 * 1024 * 1024);
    write_config(&inputs, &config);
    let config_bytes = std::fs::read(&inputs.server.config_path).unwrap();
    inputs.server.start().await;
    let session = "real-protected-state-session";
    async fn ledger(server: &Server, expected: u64) -> Value {
        server
            .wait_for(async || {
                let value = server.get("juiced/admission").await;
                let (resident, scratch, reserved) = cache_control_claims(&value);
                value["scheduler"]["memory"]["reserved_bytes"] == expected + reserved
                    && value["scheduler"]["memory"]["outstanding_bytes"] == resident
                    && value["scheduler"]["memory"]["resident_bytes"] == expected
                    && scratch == 0
            })
            .await;
        let value = server.get("juiced/admission").await;
        assert_eq!(
            value["resources"]["reservation_source"],
            "declared-protected-host"
        );
        assert_eq!(
            value["resources"]["resident_budget_bytes"],
            1024 * 1024 * 1024
        );
        assert_eq!(
            value["resources"]["reserved_bytes"],
            expected + cache_control_claims(&value).0
        );
        assert_eq!(
            value["scheduler"]["memory"]["essential_headroom_bytes"],
            64 * 1024 * 1024
        );
        assert_eq!(
            server.model(MODEL).await["resident_bytes"],
            256 * 1024 * 1024
        );
        value
    }
    let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{loaded}; host={}",
        inputs.server.get("juiced/state").await["resources"]["host_memory"]
    );
    let first_completion = real_completion(&inputs.server, false, session, None).await;
    let first = real_observation(&inputs, 768).await;
    let settled = ledger(&inputs.server, 256 * 1024 * 1024).await;
    let sample = settled["resources"]["host_memory"].clone();
    assert_eq!(sample["constraints_complete"], true, "{sample}");
    assert!(sample["reason"].is_null(), "{sample}");
    assert!(
        sample["available_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes >= 704 * 1024 * 1024),
        "{sample}"
    );
    let cache = inputs.server.get("juiced/cache").await;
    let(status,saved)=inputs.server.post("juiced/cache/save",json!({"model":MODEL,"slot":0,"session":session,"expected_generation":cache["generation"],"maximum_bytes":16*1024*1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["snapshot"]["state"], "saved");
    assert_eq!(saved["snapshot"]["writer_bound"], true);
    assert_eq!(saved["snapshot"]["active_readers"], 0);
    assert_eq!(saved["snapshot"]["retained_readers"], json!([]));
    assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    assert_eq!(saved["snapshot"]["bytes"], saved["engine"]["n_written"]);
    ledger(&inputs.server, 256 * 1024 * 1024).await;
    unload(&inputs.server).await;
    ledger(&inputs.server, 0).await;
    let first_pid = first["pid"].as_u64().unwrap();
    assert!(!Path::new(&format!("/proc/{first_pid}")).exists());
    inputs.server.restart().await;
    let (status, restored) = inputs
        .server
        .post(
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":session,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["retained_reader_cleanup_pending"], false);
    assert_eq!(restored["snapshot"]["retained_readers"], json!([]));
    assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    let continued = real_completion(&inputs.server, true, session, None).await;
    let second = real_observation(&inputs, 768).await;
    assert_ne!(second["pid"], first["pid"]);
    assert_eq!(
        second["runtime_identity"]["closure_digest"],
        first["runtime_identity"]["closure_digest"]
    );
    ledger(&inputs.server, 256 * 1024 * 1024).await;
    let view = inputs.server.get("juiced/cache").await;
    let row = view["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == saved["snapshot"]["id"])
        .unwrap();
    assert_eq!(row["active_readers"], 0);
    assert_eq!(row["retained_readers"], json!([]));
    unload(&inputs.server).await;
    ledger(&inputs.server, 0).await;
    let second_pid = second["pid"].as_u64().unwrap();
    assert!(!Path::new(&format!("/proc/{second_pid}")).exists());
    assert_eq!(
        std::fs::read(&inputs.server.config_path).unwrap(),
        config_bytes
    );
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(Path::new(bundle["root"].as_str().unwrap())),
        bundle_image
    );
    assert_eq!(digest(&engine), engine_sha);
    assert_eq!(digest(&model), model_sha);
    let manager = inputs.server.pid();
    inputs.server.stop();
    assert!(!Path::new(&format!("/proc/{manager}")).exists());
    eprintln!(
        "protected-real-state-receipt {}",
        json!({"schema_version":1,"scope":"selected tiny real llama.cpp CPU import, declared protected host ceilings and successful slot save/restart/restore/continuation; no peak calibration, output-quality, crash-reader or broad-platform qualification","model":{"sha256":model_sha,"bytes":model_metadata.len()},"engine_sha256":engine_sha,"bundle_files":bundle["files"],"profile":config["models"].as_array().unwrap().iter().find(|model|model["id"]==MODEL).unwrap()["resource_profile"],"sample":sample,"first":{"completion":first_completion,"observation":first},"second":{"completion":continued,"observation":second},"cache":{"saved":saved["engine"],"restored":restored["engine"]},"worker_exit_count":2,"manager_exit_acknowledged":true,"final_host_claims":0,"source_unchanged":true,"bundle_unchanged":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_HOST_MEMORY=1; protected cache controller and actual retained socket bytes"]
async fn protected_cache_metadata_and_socket_responses_share_host_claims_without_loading_a_model() {
    use tokio::io::AsyncWriteExt;
    assert_eq!(std::env::var("JUICED_TEST_HOST_MEMORY").as_deref(), Ok("1"));
    let (mut inputs, fingerprint) = protected_baseline().await;
    let config = protected_config(&inputs, &fingerprint, Some(256 * 1024 * 1024), 5000);
    write_config(&inputs, &config);
    let epoch = format!(
        "{:x}",
        Sha256::digest(format!(
            "juiced-auth-v1\0configured-admin\0{}",
            inputs.server.admin_key().unwrap()
        ))
    );
    let identity = json!({"execution_fingerprint":"a".repeat(64),"principal":"configured-admin","credential_epoch":epoch,"protocol":"openai.completions","session_digest":"b".repeat(64)});
    let record = json!({"identity":identity,"revision":2,"state":"saved","reserved_bytes":64,"bytes":64,"sha256":"c".repeat(64)});
    let entries = (0..16000)
        .map(|index| (format!("{index:032x}"), record.clone()))
        .collect::<serde_json::Map<_, _>>();
    let manifest = inputs
        .server
        .directory
        .path()
        .join("kv-cache/snapshots.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema_version":1,"generation":1,"entries":entries})).unwrap(),
    )
    .unwrap();
    let source = inputs.source_image();
    let config_bytes = std::fs::read(&inputs.server.config_path).unwrap();
    let manifest_sha = digest(&manifest);
    inputs.server.start().await;
    let idle = host_claims(&inputs.server, 0, 0, 0).await;
    let controller = &idle["resources"]["cache_control_memory"];
    assert_eq!(controller["state"], "accounted");
    assert_eq!(controller["active_operations"], 0);
    assert_eq!(controller["scratch_bytes"], 0);
    let resident = controller["metadata_bytes"].as_u64().unwrap();
    let scratch = controller["bounds"]["scratch_bytes"].as_u64().unwrap();
    assert!(controller["bounds"]["bootstrap_bytes"].as_u64().unwrap() > resident);
    let (status, paused) = inputs
        .server
        .post("juiced/admission", json!({"paused":true}))
        .await;
    assert!(status.is_success(), "{paused}");
    let page = inputs.server.get("juiced/cache?limit=128").await;
    assert_eq!(page["data"].as_array().unwrap().len(), 128);
    let inspection = inputs.server.get("juiced/cache/inspect?limit=128").await;
    assert_eq!(inspection["snapshots"].as_array().unwrap().len(), 128);
    assert_eq!(inspection["read_only"], true);
    assert_eq!(digest(&manifest), manifest_sha);
    no_protected_child(&inputs.server).await;
    let (status, resumed) = inputs
        .server
        .post("juiced/admission", json!({"paused":false}))
        .await;
    assert!(status.is_success(), "{resumed}");
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(1024).unwrap();
    let mut socket = socket
        .connect(
            inputs
                .server
                .base
                .trim_start_matches("http://")
                .parse()
                .unwrap(),
        )
        .await
        .unwrap();
    socket.write_all(format!("GET /v1/juiced/cache?limit=16384 HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", inputs.server.admin_key().unwrap()).as_bytes()).await.unwrap();
    let headers = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 4096);
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
        }
        String::from_utf8(bytes).unwrap()
    })
    .await
    .unwrap();
    assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap();
    assert!(
        content_length > 6 * 1024 * 1024,
        "The response must exceed a normal TCP send buffer: {content_length}"
    );
    inputs
        .server
        .wait_for(async || {
            let view = inputs.server.get("juiced/admission").await;
            view["resources"]["cache_control_memory"]["active_operations"] == 1
                && view["scheduler"]["memory"]["outstanding_bytes"] == resident + scratch
                && view["scheduler"]["memory"]["reserved_bytes"] == resident + scratch
        })
        .await;
    assert_eq!(inputs.server.get("health").await["status"], "ok");
    assert!(inputs.server.model(MODEL).await["pid"].is_null());
    assert_eq!(digest(&manifest), manifest_sha);
    drop(socket);
    inputs
        .server
        .wait_for(async || {
            let view = inputs.server.get("juiced/admission").await;
            view["resources"]["cache_control_memory"]["active_operations"] == 0
                && view["scheduler"]["memory"]["outstanding_bytes"] == resident
                && view["scheduler"]["memory"]["reserved_bytes"] == resident
        })
        .await;
    no_protected_child(&inputs.server).await;
    assert_eq!(digest(&manifest), manifest_sha);
    assert_eq!(
        std::fs::read(&inputs.server.config_path).unwrap(),
        config_bytes
    );
    assert_eq!(inputs.source_image(), source);
    inputs.server.stop();
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real GGUF assets and JUICED_TEST_HOST_MEMORY=1; imported CPU five-slot pipeline, declared host ceilings"]
async fn protected_five_real_slots_share_host_claims_without_changing_context() {
    assert_eq!(std::env::var("JUICED_TEST_HOST_MEMORY").as_deref(), Ok("1"));
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    assert!(std::fs::metadata(&model).unwrap().len() <= 64 * 1024 * 1024);
    let engine_sha = digest(&engine);
    let model_sha = digest(&model);
    let mut prepared = Inputs::prepare_from(Some(&engine), std::fs::read(&model).unwrap()).await;
    prepared.binding["max_inflight"] = json!(5);
    std::fs::write(prepared.source.join("recipe_options.json"), serde_json::to_vec(&json!({MODEL:{"ctx_size":2560},"opaque-unrelated":{"private":SOURCE_MARKER,"future":[1,{"two":2}]}})).unwrap()).unwrap();
    let (mut inputs, fingerprint) = protected_baseline_with(prepared).await;
    let source = inputs.source_image();
    let bundle = inputs.binding["execution_profile"]["runtime_bundle"].clone();
    let bundle_image = tree(Path::new(bundle["root"].as_str().unwrap()));
    let mut config = protected_config(&inputs, &fingerprint, Some(768 * 1024 * 1024), 15000);
    config["request_timeout_ms"] = json!(30000);
    let profile = &mut config["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["id"] == MODEL)
        .unwrap()["resource_profile"];
    profile["load_bytes"] = json!(256 * 1024 * 1024);
    profile["resident_bytes"] = json!(128 * 1024 * 1024);
    profile["request_bytes"] = json!(8 * 1024 * 1024);
    profile["state_bytes"] = json!(2 * 1024 * 1024);
    profile["request_limits"]["output_tokens"] = json!(64);
    write_config(&inputs, &config);
    inputs.server.start().await;
    let (status, loaded) = inputs.server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let before = inputs
        .server
        .get(&format!("juiced/context?model={MODEL}"))
        .await;
    assert_eq!(
        before["applied"]["launch_plan"]["semantic_fingerprint"],
        fingerprint
    );
    let cache = inputs.server.get("juiced/cache").await;
    for prefix in PREFIXES {
        for (operation, body) in [
            (
                "save",
                json!({"model":MODEL,"slot":0,"session":"multi-slot-rejected","expected_generation":cache["generation"],"maximum_bytes":1024}),
            ),
            (
                "restore",
                json!({"model":MODEL,"slot":0,"session":"multi-slot-rejected","snapshot":"s-00000000000000000000000000000000"}),
            ),
        ] {
            let (status, refusal) = inputs
                .server
                .json(
                    Method::POST,
                    &format!("{prefix}/juiced/cache/{operation}"),
                    Some(body),
                    inputs.server.admin_key(),
                )
                .await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{refusal}");
            assert_eq!(refusal["error"]["code"], "cache_profile_unsupported");
        }
    }
    assert_eq!(inputs.server.get("juiced/cache").await, cache);
    let receipt = support::llama::five_slot_pipeline(
        &mut inputs.server,
        MODEL,
        loaded,
        before,
        true,
        &engine,
        &model,
    )
    .await;
    assert_eq!(digest(&engine), engine_sha);
    assert_eq!(digest(&model), model_sha);
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(Path::new(bundle["root"].as_str().unwrap())),
        bundle_image
    );
    println!(
        "protected-five-slots-receipt {}",
        json!({"pipeline":receipt,"native_import":true,"bundle_files":bundle["files"],"source_unchanged":true,"bundle_unchanged":true,"multi_slot_snapshot_mutation_refused":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn protected_direct_profiles_remain_unqualified_before_host_sampling_or_spawn() {
    let mut server = Server::configured(true, |_| {}).await;
    server.stop();
    let config: juiced_service::config::Config =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    let mut config = serde_json::to_value(config).unwrap();
    config["admission"]["memory"] = json!({"mode":"protected","capacity_bytes":512 * 1024 * 1024,"essential_headroom_bytes":0,"maximum_sample_age_ms":5000});
    config["models"][0]["execution_profile"]["adapter"] = json!("llama-cpp");
    config["models"][0]["execution_profile"]["implementation_id"] = json!("llama-cpp");
    config["models"][0]["execution_profile"]["options"] =
        json!({"context_tokens":256,"gpu_layers":0,"parallel_slots":2});
    config["models"][0]["resource_profile"] = json!({"schema_version":1,"launch_fingerprint":"a".repeat(64),"priority":"interactive","completion_ms":1000,"load_bytes":16 * 1024 * 1024,"resident_bytes":8 * 1024 * 1024,"request_bytes":1024 * 1024,"state_bytes":null,"request_limits":{"input_bytes":65536,"batch":1,"output_tokens":8}});
    std::fs::write(&server.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let original = std::fs::read(&server.config_path).unwrap();
    server.start().await;
    let id = config["models"][0]["id"].as_str().unwrap();
    for prefix in PREFIXES {
        let (status, error) = server
            .json(
                Method::POST,
                &format!("{prefix}/load"),
                Some(json!({"model":id})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
        assert_eq!(error["error"]["code"], "protected_profile_unqualified");
    }
    assert_eq!(server.model(id).await["loaded"], false);
    assert_eq!(
        server.get("juiced/state").await["resources"]["active_requests"],
        0
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), original);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn protected_import_without_runtime_bundle_refuses_before_spawn() {
    let (inputs, fingerprint) = protected_baseline().await;
    let mut config = protected_config(&inputs, &fingerprint, Some(512 * 1024 * 1024), 1000);
    let model = config["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["id"] == MODEL)
        .unwrap();
    model["execution_profile"]
        .as_object_mut()
        .unwrap()
        .remove("runtime_bundle");
    write_config(&inputs, &config);
    let original = std::fs::read(&inputs.server.config_path).unwrap();
    let source = inputs.source_image();
    let native = tree(&inputs.server.directory.path().join(STORE_ROOT));
    let artifacts = tree(&inputs.server.directory.path().join("artifacts"));
    let result = support::rejected_manager(&inputs.server.config_path).await;
    assert!(
        !result.status.success(),
        "missing native runtime bundle started a manager"
    );
    assert!(result.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(
        diagnostic.contains("Native runtime bindings require an explicit verified runtime bundle"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains(SOURCE_MARKER));
    assert_eq!(inputs.source_image(), source);
    assert_eq!(
        tree(&inputs.server.directory.path().join(STORE_ROOT)),
        native
    );
    assert_eq!(
        tree(&inputs.server.directory.path().join("artifacts")),
        artifacts
    );
    assert_eq!(std::fs::read(&inputs.server.config_path).unwrap(), original);
}
