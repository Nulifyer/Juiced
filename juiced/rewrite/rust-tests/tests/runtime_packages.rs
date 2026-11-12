#![cfg(unix)]
mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use support::{
    Server, binary,
    cli::offline,
    filesystem::{digest, tree},
};

struct InstalledServer(Server);
impl std::ops::Deref for InstalledServer {
    type Target = Server;
    fn deref(&self) -> &Server {
        &self.0
    }
}
impl std::ops::DerefMut for InstalledServer {
    fn deref_mut(&mut self) -> &mut Server {
        &mut self.0
    }
}
impl Drop for InstalledServer {
    fn drop(&mut self) {
        self.0.stop();
        fn writable(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                            writable(&entry.path());
                        }
                    }
                }
            }
        }
        writable(&self.0.directory.path().join("runtime-packages"));
    }
}
async fn complete(server: &Server, model: &str, session: &str, tokens: u32) -> Value {
    let response=server.client.post(format!("{}/v1/completions",server.base)).bearer_auth(server.admin_key().unwrap()).header("x-juiced-session",session)
        .json(&json!({"model":model,"prompt":"Once upon a time","max_tokens":tokens,"temperature":0,"cache_prompt":true})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: Value = response.json().await.unwrap();
    assert_eq!(response["model"], model);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    response["usage"].clone()
}

fn import_spec(bundle: Value) -> Value {
    let root = PathBuf::from(bundle["root"].as_str().unwrap());
    let files:Vec<_>=bundle["files"].as_array().unwrap().iter().map(|file|json!({"path":file["path"],"bytes":fs::metadata(root.join(file["path"].as_str().unwrap())).unwrap().len()})).collect();
    json!({"schema_version":1,"bundle":bundle,"files":files,
        "provenance":{"builder_id":"rust-process-fixture","source_revision":"observed-unqualified-input","build_revision":"local-import-test","declared_licenses":["unknown"],"declared_sbom":null},
        "declared_target":{"operating_system":std::env::consts::OS,"architecture":std::env::consts::ARCH,"abi":"unknown","compute_paths":["cpu"]}})
}
async fn import(server: &Server, input: &Path) -> std::process::Output {
    offline(
        &server.config_path,
        &[OsStr::new("--import-runtime-bundle"), input.as_os_str()],
        Duration::from_secs(45),
    )
    .await
}
async fn package_bindings(server: &Server, id: &Value) -> u64 {
    server.get("juiced/runtime-packages").await["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package.get("id") == Some(id))
        .unwrap()["binding_leases"]
        .as_u64()
        .unwrap()
}
fn success(output: std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn one_file_source(root: &Path, engine: &Path) -> Value {
    fs::copy(engine, root.join("engine")).unwrap();
    json!({"schema_version":1,"implementation_id":"llama-cpp","root":root,"entrypoint":"engine","library_directories":[],"files":[{"path":"engine","sha256":digest(&root.join("engine")),"role":"executable"}]})
}

#[tokio::test]
async fn online_runtime_import_initializes_only_explicitly_retries_all_prefixes_and_preserves_config()
 {
    let mut server = InstalledServer(Server::prepare(true, |config| config["models"] = json!([])));
    let source = tempfile::tempdir().unwrap();
    let worker = binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let worker_before = digest(&worker);
    let input = server.directory.path().join("online-import.json");
    fs::write(
        &input,
        serde_json::to_vec(&import_spec(one_file_source(source.path(), &worker))).unwrap(),
    )
    .unwrap();
    server.start().await;
    let configured = fs::read(&server.config_path).unwrap();
    let root = server.directory.path().join("runtime-packages");
    assert!(!root.exists());
    assert_eq!(server.get("juiced/runtime-packages").await["generation"], 0);
    assert!(!root.exists());
    for body in [
        json!({"specification_file":"relative.json","expected_generation":0}),
        json!({"specification_file":input,"expected_generation":99}),
    ] {
        let (status, _) = server.post("juiced/runtime-packages/import", body).await;
        assert!(matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::CONFLICT
        ));
        assert!(!root.exists());
    }
    let mut installed = Value::Null;
    let mut retained = None;
    for (index, prefix) in support::PREFIXES.into_iter().enumerate() {
        let generation = server.get("juiced/runtime-packages").await["generation"].clone();
        let (status, receipt) = server
            .json(
                reqwest::Method::POST,
                &format!("{prefix}/juiced/runtime-packages/import"),
                Some(json!({"specification_file":input,"expected_generation":generation})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{receipt}");
        assert_eq!(receipt["installed"], true);
        assert_eq!(receipt["newly_installed"], index == 0);
        if index == 0 {
            installed = receipt;
            fs::remove_file(source.path().join("engine")).unwrap();
            retained = Some(tree(&root));
        } else {
            assert_eq!(receipt["package"], installed["package"]);
        }
        assert_eq!(fs::read(&server.config_path).unwrap(), configured);
        assert_eq!(tree(&root), *retained.as_ref().unwrap());
        assert!(
            server.get("juiced/catalog").await["data"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    server.restart().await;
    assert_eq!(server.get("juiced/runtime-packages").await["generation"], 2);
    assert_eq!(package_bindings(&server, &installed["package"]).await, 0);
    let (status, retry) = server
        .post(
            "juiced/runtime-packages/import",
            json!({"specification_file":input,"expected_generation":2}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry["newly_installed"], false);
    assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    assert_eq!(tree(&root), retained.unwrap());
    assert_eq!(digest(&worker), worker_before);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn accepted_online_installation_survives_its_client_disconnect_and_releases_its_actual_owner()
{
    detached_runtime_installation(false).await;
}

#[cfg(target_os = "linux")]
async fn detached_runtime_installation(protected: bool) {
    use tokio::io::AsyncWriteExt;
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["models"] = json!([]);
        if protected {
            config["admission"] = json!({"memory":{"mode":"protected","capacity_bytes":512u64*1024*1024,"essential_headroom_bytes":0,"maximum_sample_age_ms":5000}});
        }
    }));
    let source = tempfile::tempdir().unwrap();
    let input = server.directory.path().join("detached-import.json");
    fs::write(
        &input,
        serde_json::to_vec(&import_spec(one_file_source(
            source.path(),
            &binary("juiced-test-worker", "JUICED_TEST_WORKER"),
        )))
        .unwrap(),
    )
    .unwrap();
    server.start().await;
    let configured = fs::read(&server.config_path).unwrap();
    let body =
        serde_json::to_vec(&json!({"specification_file":input,"expected_generation":0})).unwrap();
    let mut socket = tokio::net::TcpStream::connect(server.base.trim_start_matches("http://"))
        .await
        .unwrap();
    socket.write_all(format!("POST /v1/juiced/runtime-packages/import HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",server.admin_key().unwrap(),body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    server
        .wait_for(async || {
            let (status, inventory) = server
                .json(
                    reqwest::Method::GET,
                    "/v1/juiced/runtime-packages",
                    None,
                    server.admin_key(),
                )
                .await;
            status == StatusCode::OK && inventory["installation_active"] == true
        })
        .await;
    drop(socket);
    server
        .wait_for(async || {
            let (status, inventory) = server
                .json(
                    reqwest::Method::GET,
                    "/v1/juiced/runtime-packages",
                    None,
                    server.admin_key(),
                )
                .await;
            status == StatusCode::OK
                && inventory["installation_active"] == false
                && inventory["packages"].as_array().is_some_and(|packages| {
                    packages.len() == 1 && packages[0]["phase"] == "installed"
                })
        })
        .await;
    let (status, retry) = server
        .post(
            "juiced/runtime-packages/import",
            json!({"specification_file":input,"expected_generation":2}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry["newly_installed"], false);
    assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    assert_eq!(
        server.get("juiced/runtime-packages").await["installation_active"],
        false
    );
    if protected {
        server.wait_for(async || server.get("juiced/admission").await["resources"]["runtime_control_memory"]["active_operations"] == 0).await;
        let view = server.get("juiced/admission").await;
        let runtime = &view["resources"]["runtime_control_memory"];
        assert_eq!(runtime["state"], "accounted");
        assert_eq!(
            runtime["metadata_bytes"],
            runtime["bounds"]["metadata_bytes"]
        );
        assert_eq!(runtime["binding_bytes"], 0);
        assert_eq!(runtime["scratch_bytes"], 0);
        assert_eq!(
            view["scheduler"]["memory"]["outstanding_bytes"],
            runtime["metadata_bytes"].as_u64().unwrap()
                + view["resources"]["cache_control_memory"]["metadata_bytes"]
                    .as_u64()
                    .unwrap()
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn online_runtime_addition_preserves_a_live_managed_worker_and_its_cache_across_restart() {
    const MODEL: &str = "online-runtime-survivor";
    const SESSION: &str = "online-runtime-survivor-session";
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["cache_enabled"] = json!(true);
    }));
    server.isolate_loader_environment();
    let source = tempfile::tempdir().unwrap();
    let input = server.directory.path().join("online-import.json");
    fs::write(
        &input,
        serde_json::to_vec(&import_spec(one_file_source(
            source.path(),
            &binary("juiced-test-worker", "JUICED_TEST_WORKER"),
        )))
        .unwrap(),
    )
    .unwrap();
    server.start().await;
    let (status, first) = server
        .post(
            "juiced/runtime-packages/import",
            json!({"specification_file":input,"expected_generation":0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    fs::remove_file(source.path().join("engine")).unwrap();
    let (status, registered) = server.post("models/register",json!({"id":MODEL,"model_file":server.directory.path().join("fixture weights.gguf"),"resident_bytes":64,"max_inflight":1,"operations":["completions"],"execution_profile":{"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":first["package"],"args":["--host","127.0.0.1","--port","{port}","--model","{model}","--parallel","1","--ctx-size","256","--n-gpu-layers","0","--slot-save-path","{cache}"],"options":{},"placement":"unresolved"}})).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert_eq!(registered["policy"], "disabled");
    let (status, enabled) = server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"on-demand","expected_revision":registered["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    complete(&server, MODEL, SESSION, 2).await;
    let before = server.get(&format!("juiced/context?model={MODEL}")).await;
    let pid = before["applied"]["pid"].as_u64().unwrap();
    let cache = server.get("juiced/cache").await;
    let (status,saved) = server.post("juiced/cache/save",json!({"model":MODEL,"slot":0,"session":SESSION,"expected_generation":cache["generation"],"maximum_bytes":4096})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let original = tree(&PathBuf::from(first["bundle"]["root"].as_str().unwrap()));
    let configured = fs::read(&server.config_path).unwrap();
    let mut second_spec = import_spec(first["bundle"].clone());
    second_spec["provenance"]["build_revision"] = json!("online-new-package-same-live-source");
    fs::write(&input, serde_json::to_vec(&second_spec).unwrap()).unwrap();
    let (installation, ()) = tokio::join!(
        server.post(
            "juiced/runtime-packages/import",
            json!({"specification_file":input,"expected_generation":2})
        ),
        async {
            for _ in 0..3 {
                assert_eq!(server.get("health").await["status"], "ok");
                complete(&server, MODEL, SESSION, 2).await;
            }
        }
    );
    assert_eq!(installation.0, StatusCode::OK, "{}", installation.1);
    assert_ne!(installation.1["package"], first["package"]);
    assert_eq!(server.model(MODEL).await["pid"], pid);
    assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    assert_eq!(package_bindings(&server, &first["package"]).await, 1);
    assert_eq!(
        package_bindings(&server, &installation.1["package"]).await,
        0
    );
    assert_eq!(
        tree(&PathBuf::from(first["bundle"]["root"].as_str().unwrap())),
        original
    );
    let (status, restored) = server
        .post(
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    server.restart().await;
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    let (status, restored) = server
        .post(
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    let resumed = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_eq!(
        resumed["applied"]["launch_plan"]["semantic_fingerprint"],
        before["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    complete(&server, MODEL, SESSION, 2).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn online_runtime_import_refuses_another_replaced_live_store_without_writing_either_tree() {
    let mut server = InstalledServer(Server::prepare(true, |config| config["models"] = json!([])));
    let source = tempfile::tempdir().unwrap();
    let input = server.directory.path().join("root-scope-import.json");
    let mut request = import_spec(one_file_source(
        source.path(),
        &binary("juiced-test-worker", "JUICED_TEST_WORKER"),
    ));
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    server.start().await;
    let (status, first) = server
        .post(
            "juiced/runtime-packages/import",
            json!({"specification_file":input,"expected_generation":0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    request["provenance"]["build_revision"] = json!("must-not-reserve-under-foreign-cache");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let package_root = server.directory.path().join("runtime-packages");
    let packages = tree(&package_root);
    let configured = fs::read(&server.config_path).unwrap();
    let foreign =
        support::filesystem::ReplacedRoot::replace(&server.directory.path().join("cache"));
    let (status, error) = server
        .post(
            "juiced/runtime-packages/import",
            json!({"specification_file":input,"expected_generation":2}),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert_eq!(error["error"]["code"], "storage_root_unavailable");
    assert_eq!(tree(&package_root), packages);
    assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    foreign.assert_foreign_unchanged();
    foreign.restore();
    assert_eq!(server.get("juiced/runtime-packages").await["generation"], 2);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn online_runtime_import_refuses_paused_and_unfunded_protected_work_before_source_access() {
    for protected in [false, true] {
        let mut server = InstalledServer(Server::prepare(true, |config| {
            config["models"] = json!([]);
            if protected {
                config["admission"] = json!({"memory":{"mode":"protected","capacity_bytes":32u64*1024*1024,"essential_headroom_bytes":0,"maximum_sample_age_ms":5000}});
            }
        }));
        server.start().await;
        if !protected {
            assert_eq!(
                server
                    .post("juiced/admission", json!({"paused":true}))
                    .await
                    .0,
                StatusCode::OK
            );
        }
        let input = server.directory.path().join("does-not-exist.json");
        let configured = fs::read(&server.config_path).unwrap();
        let (status, error) = server
            .post(
                "juiced/runtime-packages/import",
                json!({"specification_file":input,"expected_generation":0}),
            )
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
        assert_eq!(
            error["error"]["code"],
            if protected {
                "host_memory_exhausted"
            } else {
                "runtime_import_paused"
            }
        );
        assert!(!server.directory.path().join("runtime-packages").exists());
        assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_HOST_MEMORY=1 and complete observed host headroom; selected runtime controller declared layout ceilings, not worker/RSS calibration"]
async fn protected_runtime_import_retains_metadata_and_binding_claims_across_retries_and_restart() {
    assert_eq!(std::env::var("JUICED_TEST_HOST_MEMORY").as_deref(), Ok("1"));
    const MODEL: &str = "protected-runtime-binding";
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["admission"] = json!({"memory":{"mode":"protected","capacity_bytes":512u64*1024*1024,"essential_headroom_bytes":0,"maximum_sample_age_ms":5000}});
    }));
    let source = tempfile::tempdir().unwrap();
    let worker = binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let original = digest(&worker);
    let input = server
        .directory
        .path()
        .join("protected-runtime-import.json");
    fs::write(
        &input,
        serde_json::to_vec(&import_spec(one_file_source(source.path(), &worker))).unwrap(),
    )
    .unwrap();
    server.start().await;
    let configured = fs::read(&server.config_path).unwrap();
    let root = server.directory.path().join("runtime-packages");
    let empty = server.get("juiced/admission").await;
    assert_eq!(
        empty["resources"]["runtime_control_memory"]["metadata_bytes"],
        0
    );
    assert!(!root.exists());
    let mut package = Value::Null;
    for (index, prefix) in support::PREFIXES.into_iter().enumerate() {
        let generation = server.get("juiced/runtime-packages").await["generation"].clone();
        let (status, receipt) = server
            .json(
                reqwest::Method::POST,
                &format!("{prefix}/juiced/runtime-packages/import"),
                Some(json!({"specification_file":input,"expected_generation":generation})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{receipt}");
        assert_eq!(receipt["newly_installed"], index == 0);
        if index == 0 {
            package = receipt["package"].clone();
            fs::remove_file(source.path().join("engine")).unwrap();
        }
        assert_eq!(receipt["package"], package);
        assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    }
    let payloads = tree(&root);
    server.wait_for(async || server.get("juiced/admission").await["resources"]["runtime_control_memory"]["active_operations"] == 0).await;
    let idle = server.get("juiced/admission").await;
    let metadata = idle["resources"]["runtime_control_memory"]["metadata_bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(
        idle["resources"]["runtime_control_memory"]["state"],
        "accounted"
    );
    assert_eq!(
        idle["resources"]["runtime_control_memory"]["bounds"]["metadata_bytes"],
        metadata
    );
    assert!(metadata > 0);
    assert_eq!(
        idle["resources"]["runtime_control_memory"]["scratch_bytes"],
        0
    );
    let cache = idle["resources"]["cache_control_memory"]["metadata_bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(
        idle["scheduler"]["memory"]["outstanding_bytes"],
        metadata + cache
    );
    let (status, registered) = server.post("models/register",json!({"id":MODEL,"model_file":server.directory.path().join("fixture weights.gguf"),"resident_bytes":64,"max_inflight":1,"operations":["completions"],"execution_profile":{"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":package,"args":["--host","127.0.0.1","--port","{port}","--model","{model}","--parallel","1","--ctx-size","256","--n-gpu-layers","0","--slot-save-path","{cache}"],"options":{},"placement":"unresolved"}})).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert_eq!(registered["policy"], "disabled");
    assert_eq!(registered["loaded"], false);
    let bound = server.get("juiced/admission").await;
    let binding = bound["resources"]["runtime_control_memory"]["binding_bytes"]
        .as_u64()
        .unwrap();
    assert!(binding > 0);
    assert_eq!(
        bound["resources"]["runtime_control_memory"]["binding_count"],
        1
    );
    assert_eq!(
        bound["resources"]["runtime_control_memory"]["active_operations"],
        0
    );
    assert_eq!(
        bound["scheduler"]["memory"]["outstanding_bytes"],
        metadata + cache + binding
    );
    let saved = fs::read(&server.config_path).unwrap();
    server.restart().await;
    assert_eq!(fs::read(&server.config_path).unwrap(), saved);
    assert_eq!(tree(&root), payloads);
    let recovered = server.get("juiced/admission").await;
    assert_eq!(
        recovered["resources"]["runtime_control_memory"]["metadata_bytes"],
        metadata
    );
    assert_eq!(
        recovered["resources"]["runtime_control_memory"]["binding_count"],
        1
    );
    assert_eq!(
        recovered["resources"]["runtime_control_memory"]["binding_bytes"],
        binding
    );
    assert_eq!(
        recovered["scheduler"]["memory"]["outstanding_bytes"],
        metadata + cache + binding
    );
    let view = server.model(MODEL).await;
    assert_eq!(view["policy"], "disabled");
    assert_eq!(view["loaded"], false);
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":MODEL,"expected_revision":view["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    server.wait_for(async || server.get("juiced/admission").await["resources"]["runtime_control_memory"]["binding_count"] == 0).await;
    let final_view = server.get("juiced/admission").await;
    assert_eq!(
        final_view["scheduler"]["memory"]["outstanding_bytes"],
        metadata + cache
    );
    assert_eq!(
        final_view["resources"]["runtime_control_memory"]["active_operations"],
        0
    );
    assert_eq!(
        final_view["resources"]["runtime_control_memory"]["binding_bytes"],
        0
    );
    assert_eq!(tree(&root), payloads);
    server.stop();
    assert_eq!(digest(&worker), original);
    detached_runtime_installation(true).await;
}

#[tokio::test]
async fn missing_runtime_control_refuses_startup_and_explicit_import_without_recreating_authority()
{
    let server = InstalledServer(Server::prepare(true, |config| config["models"] = json!([])));
    let source = tempfile::tempdir().unwrap();
    let input = server.directory.path().join("control-refusal.json");
    fs::write(
        &input,
        serde_json::to_vec(&import_spec(one_file_source(
            source.path(),
            &binary("juiced-test-worker", "JUICED_TEST_WORKER"),
        )))
        .unwrap(),
    )
    .unwrap();
    success(import(&server, &input).await);
    let root = server.directory.path().join("runtime-packages");
    fs::remove_file(root.join(".juiced-store.lock")).unwrap();
    let before = tree(&root);
    let config = fs::read(&server.config_path).unwrap();
    let rejected = support::rejected_manager(&server.config_path).await;
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("package_store_unavailable"));
    let rejected = import(&server, &input).await;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("package_store_unavailable"));
    assert_eq!(fs::read(&server.config_path).unwrap(), config);
    assert_eq!(tree(&root), before);
}

#[tokio::test]
async fn offline_runtime_cli_installs_retries_and_runs_the_copied_worker_through_cache_restart_without_source()
 {
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["cache_enabled"] = json!(true);
        config["models"] = json!([]);
    }));
    server.isolate_loader_environment();
    let source = tempfile::tempdir().unwrap();
    let worker = binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let original = digest(&worker);
    let bundle = one_file_source(source.path(), &worker);
    let copied_source = digest(&source.path().join("engine"));
    assert_eq!(original, copied_source);
    let inputs = tempfile::tempdir().unwrap();
    let input = inputs.path().join("import.json");
    fs::write(&input, serde_json::to_vec(&import_spec(bundle)).unwrap()).unwrap();
    let configured = fs::read(&server.config_path).unwrap();
    let installed = success(import(&server, &input).await);
    assert_eq!(installed["installed"], true);
    assert_eq!(installed["newly_installed"], true);
    assert_eq!(installed["compatibility"], "unqualified");
    assert_eq!(fs::read(&server.config_path).unwrap(), configured);
    assert_eq!(digest(&worker), original);
    let root = PathBuf::from(installed["bundle"]["root"].as_str().unwrap());
    assert_eq!(digest(&root.join("engine")), original);
    let manifest = server
        .directory
        .path()
        .join("runtime-packages/packages.json");
    let head = fs::read(&manifest).unwrap();
    fs::remove_file(source.path().join("engine")).unwrap();
    let repeated = success(import(&server, &input).await);
    assert_eq!(repeated["newly_installed"], false);
    assert_eq!(repeated["package"], installed["package"]);
    assert_eq!(fs::read(&manifest).unwrap(), head);
    server.config["models"] = json!([{"id":"installed-worker","model_file":server.directory.path().join("fixture weights.gguf"),"provider":"llama-cpp","runtime_bundle":installed["bundle"],"command":root.join("engine"),
        "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--parallel","1","--ctx-size","256","--n-gpu-layers","0","--slot-save-path","{cache}"],
        "resident_bytes":64,"max_inflight":1,"operations":["completions"],"policy":"on-demand"}]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    server.start().await;
    let (status, loaded) = server
        .post("load", json!({"model":"installed-worker"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    complete(&server, "installed-worker", "installed-runtime-session", 2).await;
    let before = server.get("juiced/context?model=installed-worker").await;
    let cache = server.get("juiced/cache").await;
    let (status,saved)=server.post("juiced/cache/save",json!({"model":"installed-worker","slot":0,"session":"installed-runtime-session","expected_generation":cache["generation"],"maximum_bytes":4096})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    let live_tree = tree(&server.directory.path().join("runtime-packages"));
    let rejected = import(&server, &input).await;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("owner_busy"));
    assert_eq!(
        tree(&server.directory.path().join("runtime-packages")),
        live_tree
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"installed-worker"}))
            .await
            .0,
        StatusCode::OK
    );
    server.restart().await;
    let (status,restored)=server.post("juiced/cache/restore",json!({"model":"installed-worker","slot":0,"session":"installed-runtime-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    complete(&server, "installed-worker", "installed-runtime-session", 2).await;
    let resumed = server.get("juiced/context?model=installed-worker").await;
    assert_eq!(
        before["applied"]["launch_plan"]["semantic_fingerprint"],
        resumed["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_ne!(before["applied"]["pid"], resumed["applied"]["pid"]);
    server.stop();
    assert_eq!(digest(&root.join("engine")), original);
    assert_eq!(digest(&worker), original);
    assert_eq!(fs::read(&manifest).unwrap(), head);
}

#[tokio::test]
async fn offline_runtime_cli_rejects_conflicting_flags_and_invalid_source_without_config_or_model_changes()
 {
    let server = Server::prepare(false, |config| config["models"] = json!([]));
    let source = tempfile::tempdir().unwrap();
    let input = source.path().join("invalid-private-import.json");
    fs::write(
        &input,
        b"{\"schema_version\":1,\"private-input\":\"DO_NOT_ECHO_RUNTIME_INPUT\"}",
    )
    .unwrap();
    let config = fs::read(&server.config_path).unwrap();
    for arguments in [
        vec![OsStr::new("--import-runtime-bundle"), input.as_os_str()],
        vec![
            OsStr::new("--import-runtime-bundle"),
            input.as_os_str(),
            OsStr::new("--listen"),
            OsStr::new("127.0.0.1:0"),
        ],
        vec![
            OsStr::new("--import-runtime-bundle"),
            input.as_os_str(),
            OsStr::new("--backup-settings-copy"),
            source.path().as_os_str(),
        ],
        vec![
            OsStr::new("--import-runtime-bundle"),
            input.as_os_str(),
            OsStr::new("--import-native-settings-copy"),
            source.path().as_os_str(),
        ],
    ] {
        let rejected = offline(&server.config_path, &arguments, Duration::from_secs(10)).await;
        assert!(!rejected.status.success());
        assert!(rejected.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&rejected.stderr).contains("DO_NOT_ECHO_RUNTIME_INPUT"));
        assert_eq!(fs::read(&server.config_path).unwrap(), config);
        assert!(!server.directory.path().join("runtime-packages").exists());
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real CPU llama-server/GGUF assets; installed runtime promotion/rollback and cache lifecycle, not model quality or hardware qualification"]
async fn tiny_gguf_installed_runtime_copy_load_cache_restart_and_continue_without_source() {
    const MODEL: &str = "installed-tiny-real-runtime";
    const SESSION: &str = "installed-real-runtime-session";
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let engine_before = digest(&engine);
    let model_before = digest(&model);
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["cache_enabled"] = json!(true);
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
    }));
    server.isolate_loader_environment();
    let source = tempfile::tempdir().unwrap();
    let (_, bundle) = support::llama::flat_cpu_bundle(&engine, source.path());
    let member_count = bundle["files"].as_array().unwrap().len();
    assert!(member_count > 1);
    let source_before = tree(source.path());
    let input = server.directory.path().join("runtime-import.json");
    let mut request = import_spec(bundle);
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let installed = success(import(&server, &input).await);
    request["provenance"]["build_revision"] = json!("second-real-runtime-label-same-closure");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let replacement = success(import(&server, &input).await);
    assert_ne!(installed["package"], replacement["package"]);
    assert_eq!(installed["bundle_digest"], replacement["bundle_digest"]);
    let root = PathBuf::from(installed["bundle"]["root"].as_str().unwrap());
    assert_eq!(tree(&root), source_before);
    assert_eq!(tree(source.path()), source_before);
    assert_eq!(digest(&root.join("llama-server")), engine_before);
    let package_head = fs::read(
        server
            .directory
            .path()
            .join("runtime-packages/packages.json"),
    )
    .unwrap();
    for entry in fs::read_dir(source.path()).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        fs::remove_file(entry.path()).unwrap();
    }
    assert!(fs::read_dir(source.path()).unwrap().next().is_none());
    server.config["schema_version"] = json!(2);
    server.config["models"] = json!([{"id":MODEL,"model_file":model,"execution_profile":{"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":installed["package"],
        "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--threads","2","--slots","--n-gpu-layers","0","--slot-save-path","{cache}"],"options":{},"placement":"unresolved"},
        "resident_bytes":512u64*1024*1024,"max_inflight":1,"operations":["completions"],"policy":"on-demand"}]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    let original_configuration = fs::read(&server.config_path).unwrap();
    server.start().await;
    let first_usage = complete(&server, MODEL, SESSION, 1).await;
    assert_eq!(first_usage["completion_tokens"], 1);
    let before = server.get(&format!("juiced/context?model={MODEL}")).await;
    let first_pid = before["applied"]["pid"].as_u64().unwrap();
    let cache = server.get("juiced/cache").await;
    let (status,saved)=server.post("juiced/cache/save",json!({"model":MODEL,"slot":0,"session":SESSION,"expected_generation":cache["generation"],"maximum_bytes":16*1024*1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    assert_eq!(
        server.post("unload", json!({"model":MODEL})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        fs::read(&server.config_path).unwrap(),
        original_configuration
    );
    let path = format!("models/{MODEL}/runtime");
    let current = server.get(&path).await;
    let (status, promoted) = server.post(&path, json!({"expected_revision":current["revision"],"change":{"action":"promote","package":replacement["package"]}})).await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    server.restart().await;
    let (status, mismatch) = server
        .post(
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "snapshot_identity_mismatch");
    let other = server.get(&format!("juiced/context?model={MODEL}")).await;
    let replacement_pid = other["applied"]["pid"].as_u64().unwrap();
    assert_ne!(
        other["applied"]["launch_plan"]["semantic_fingerprint"],
        before["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_eq!(
        server.post("unload", json!({"model":MODEL})).await.0,
        StatusCode::OK
    );
    let (status, rolled) = server
        .post(
            &path,
            json!({"expected_revision":promoted["revision"],"change":{"action":"rollback"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rolled}");
    assert_eq!(rolled["current"], installed["package"]);
    assert_eq!(rolled["previous"], replacement["package"]);
    let saved_config = fs::read(&server.config_path).unwrap();
    server.restart().await;
    let (status, restored) = server
        .post(
            "juiced/cache/restore",
            json!({"model":MODEL,"slot":0,"session":SESSION,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    let continued_usage = complete(&server, MODEL, SESSION, 1).await;
    assert_eq!(continued_usage["completion_tokens"], 1);
    let resumed = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_eq!(
        before["applied"]["launch_plan"]["semantic_fingerprint"],
        resumed["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    let second_pid = resumed["applied"]["pid"].as_u64().unwrap();
    assert_ne!(first_pid, second_pid);
    let manager = server.pid();
    server.stop();
    for pid in [first_pid, replacement_pid, second_pid, u64::from(manager)] {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    assert_eq!(fs::read(&server.config_path).unwrap(), saved_config);
    assert_eq!(
        fs::read(
            server
                .directory
                .path()
                .join("runtime-packages/packages.json")
        )
        .unwrap(),
        package_head
    );
    assert_eq!(digest(&engine), engine_before);
    assert_eq!(digest(&model), model_before);
    assert_eq!(tree(&root), source_before);
    println!(
        "installed-real-runtime-receipt {}",
        json!({"scope":"Selected Linux CPU exact runtime closure copied into a private versioned package and used without its source for actual GGUF cache/restart continuation. Includes explicit managed-package promotion/rollback; no output-quality, hardware/ABI qualification, outside-writer fence, online acquisition or reclamation claim.",
        "package":installed["package"],"replacement_package":replacement["package"],"promotion_and_rollback":true,"members":member_count,"bytes":installed["reserved_bytes"],"bundle_digest":installed["bundle_digest"],"engine_sha256":engine_before,"model_sha256":model_before,
        "first_usage":first_usage,"continued_usage":continued_usage,"saved_bytes":saved["engine"]["n_written"],"restored_bytes":restored["engine"]["n_read"],"inputs_and_post_rollback_configuration_unchanged":true,"original_runtime_source_unavailable":true,"owned_processes_exited":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real CPU whisper-server/tiny-model/bundle assets; installed speech runtime lifecycle, not transcription quality"]
async fn tiny_whisper_installed_runtime_transcribes_after_source_removal_promotion_restart_and_rollback()
 {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_WHISPER_SERVER").expect("set JUICED_TEST_WHISPER_SERVER"),
    );
    let model = PathBuf::from(
        std::env::var_os("JUICED_TEST_WHISPER_MODEL").expect("set JUICED_TEST_WHISPER_MODEL"),
    );
    let manifest = PathBuf::from(
        std::env::var_os("JUICED_TEST_WHISPER_BUNDLE").expect("set JUICED_TEST_WHISPER_BUNDLE"),
    );
    let wav = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .join("test/test_speech_de.wav");
    let mut receipt = installed_speech_runtime_pipeline(engine, model, manifest, wav).await;
    receipt["scope"] = json!(
        "Selected Linux CPU Whisper exact installed closure, all-prefix JSON/text transcription, managed promotion/restart/rollback. No transcription quality, ABI/hardware qualification, online acquisition or reclamation claim."
    );
    println!("installed-real-speech-runtime-receipt {receipt}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn managed_speech_runtime_pipeline_uses_installed_files_and_keeps_exact_identity_across_restart()
 {
    let assets = tempfile::tempdir().unwrap();
    let source = assets.path().join("source");
    fs::create_dir(&source).unwrap();
    let mut bundle = one_file_source(&source, &binary("juiced-test-worker", "JUICED_TEST_WORKER"));
    bundle["implementation_id"] = json!("whisper-cpp");
    fs::write(
        source.join("NOTICE"),
        "Synthetic subprocess fixture, not model inference.\n",
    )
    .unwrap();
    bundle["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"NOTICE","sha256":digest(&source.join("NOTICE")),"role":"resource"}));
    let manifest = assets.path().join("bundle.json");
    fs::write(&manifest, serde_json::to_vec(&bundle).unwrap()).unwrap();
    let model = assets.path().join("model.bin");
    fs::write(&model, "Synthetic model fixture bytes.\n").unwrap();
    let wav = assets.path().join("audio.wav");
    fs::write(&wav, support::speech::audio()).unwrap();
    let mut receipt =
        installed_speech_runtime_pipeline(source.join("engine"), model, manifest, wav).await;
    receipt["scope"] = json!(
        "Synthetic Whisper-adapter subprocess pipeline and exact managed runtime lifecycle. No actual model inference or quality qualification."
    );
    println!("installed-synthetic-speech-runtime-receipt {receipt}");
}

#[cfg(target_os = "linux")]
async fn installed_speech_runtime_pipeline(
    engine: PathBuf,
    model: PathBuf,
    manifest: PathBuf,
    wav: PathBuf,
) -> Value {
    use support::speech::{self, Part};

    const MODEL: &str = "installed-speech-runtime";
    let model_before = digest(&model);
    let engine_before = digest(&engine);
    let manifest_before = digest(&manifest);
    let mut bundle: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(bundle["implementation_id"], "whisper-cpp");
    serde_json::from_value::<juiced_service::runtime_bundle::BundleSpec>(bundle.clone())
        .unwrap()
        .validate(juiced_service::config::Provider::WhisperCpp)
        .unwrap();
    let original_root = PathBuf::from(bundle["root"].as_str().unwrap());
    let entrypoint = PathBuf::from(bundle["entrypoint"].as_str().unwrap());
    assert_eq!(
        fs::canonicalize(original_root.join(&entrypoint)).unwrap(),
        fs::canonicalize(&engine).unwrap()
    );
    let source = tempfile::tempdir().unwrap();
    let files = bundle["files"].as_array().unwrap().clone();
    assert!(files.len() > 1);
    for file in &files {
        let path = Path::new(file["path"].as_str().unwrap());
        let target = source.path().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        assert_eq!(digest(&original_root.join(path)), file["sha256"]);
        fs::copy(original_root.join(path), &target).unwrap();
        assert_eq!(digest(&target), file["sha256"]);
    }
    for library in bundle["library_directories"].as_array().unwrap() {
        let path = Path::new(library.as_str().unwrap());
        fs::create_dir_all(source.path().join(path)).unwrap();
    }
    bundle["root"] = json!(source.path());
    let source_before = tree(source.path());
    let audio_before = digest(&wav);
    let audio = fs::read(&wav).unwrap();
    assert!(audio.starts_with(b"RIFF"));
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(60000);
    }));
    server.isolate_loader_environment();
    server.client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let input = server.directory.path().join("runtime-import.json");
    let mut request = import_spec(bundle);
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let installed = success(import(&server, &input).await);
    request["provenance"]["build_revision"] = json!("second-speech-runtime-label-same-closure");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let replacement = success(import(&server, &input).await);
    assert_ne!(installed["package"], replacement["package"]);
    assert_eq!(installed["bundle_digest"], replacement["bundle_digest"]);
    let roots: Vec<_> = [&installed, &replacement]
        .into_iter()
        .map(|package| PathBuf::from(package["bundle"]["root"].as_str().unwrap()))
        .collect();
    for root in &roots {
        assert_eq!(tree(root), source_before);
    }
    assert_eq!(tree(source.path()), source_before);
    fs::remove_dir_all(source.path()).unwrap();
    assert!(!source.path().exists());
    let head = server
        .directory
        .path()
        .join("runtime-packages/packages.json");
    let head_before = fs::read(&head).unwrap();
    server.config["schema_version"] = json!(2);
    server.config["models"] = json!([{"id":MODEL,"model_file":model,"execution_profile":{
        "schema_version":1,"adapter":"whisper-cpp","implementation_id":"whisper-cpp","runtime_package":installed["package"],
        "args":["--host","127.0.0.1","--port","{port}","-m","{model}","--threads","2","--no-gpu"],"options":{},"placement":"unresolved"},
        "resident_bytes":256u64*1024*1024,"max_inflight":1,"operations":["audio/transcriptions"],"policy":"on-demand"}]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    let path = format!("models/{MODEL}/runtime");
    let mut fingerprints = Vec::new();
    let mut worker_pids = Vec::new();
    let mut manager_pids = Vec::new();
    let mut calls = 0;
    for phase in 0..3 {
        let package = if phase == 1 { &replacement } else { &installed };
        let root = if phase == 1 { &roots[1] } else { &roots[0] };
        let saved_configuration = fs::read(&server.config_path).unwrap();
        server.start().await;
        manager_pids.push(server.pid());
        let inventory = server.get("juiced/runtime-packages").await;
        assert_eq!(inventory["packages"].as_array().unwrap().len(), 2);
        assert!(
            inventory["packages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["declared_host_match"] == true && p["compatibility"] == "unqualified")
        );
        let view = server.get(&path).await;
        assert_eq!(view["current"], package["package"]);
        assert_eq!(view["adapter"], "whisper-cpp");
        assert_eq!(view["policy"], "on-demand");
        let (status, loaded) = server.post("load", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
        assert_eq!(loaded["implementation_id"], "whisper-cpp");
        assert_eq!(loaded["runtime_package"], package["package"]);
        assert_eq!(loaded["runtime_identity"]["state"], "observed");
        assert_eq!(
            loaded["runtime_identity"]["bundle_digest"],
            installed["bundle_digest"]
        );
        let pid = loaded["pid"].as_u64().unwrap();
        assert!(!worker_pids.contains(&pid));
        worker_pids.push(pid);
        assert_eq!(
            fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
            root.join(&entrypoint)
        );
        let context = server.get(&format!("juiced/context?model={MODEL}")).await;
        assert_eq!(
            context["applied"]["launch_plan"]["runtime_package"],
            package["package"]
        );
        let fingerprint = context["applied"]["launch_plan"]["semantic_fingerprint"]
            .as_str()
            .unwrap()
            .to_owned();
        fingerprints.push(fingerprint);
        for (index, prefix) in support::PREFIXES.into_iter().enumerate() {
            let format = if index % 2 == 0 { "json" } else { "text" };
            let response = speech::send(
                &server,
                &format!("{prefix}/audio/transcriptions"),
                &[
                    Part::Text("model", MODEL),
                    Part::Text("language", "de"),
                    Part::Text("response_format", format),
                    Part::File("file", "test_speech_de.wav", "audio/wav", &audio),
                ],
                server.read_key(),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "phase={phase} prefix={prefix}"
            );
            if format == "json" {
                assert!(response.json::<Value>().await.unwrap()["text"].is_string());
            } else {
                assert!(
                    response.headers()["content-type"]
                        .to_str()
                        .unwrap()
                        .starts_with("text/plain")
                );
                response.text().await.unwrap();
            }
            assert_eq!(server.model(MODEL).await["pid"], pid);
            calls += 1;
        }
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
        let (status, blocked) = server
            .post(
                &path,
                json!({"expected_revision":view["revision"],"change":{"action":"rollback"}}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
        assert_eq!(blocked["error"]["code"], "worker_in_use");
        assert_eq!(fs::read(&server.config_path).unwrap(), saved_configuration);
        let (status, unloaded) = server.post("unload", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{unloaded}");
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
        if phase < 2 {
            let change = if phase == 0 {
                json!({"action":"promote","package":replacement["package"]})
            } else {
                json!({"action":"rollback"})
            };
            let (status, changed) = server
                .post(
                    &path,
                    json!({"expected_revision":view["revision"],"change":change}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{changed}");
            assert_eq!(
                &changed["current"],
                if phase == 0 {
                    &replacement["package"]
                } else {
                    &installed["package"]
                }
            );
            assert_eq!(changed["previous"], package["package"]);
            assert_eq!(changed["policy"], "on-demand");
            assert_eq!(changed["worker_in_use"], false);
            assert!(!server.model(MODEL).await["loaded"].as_bool().unwrap());
        }
        server.stop();
        assert_eq!(fs::read(&head).unwrap(), head_before);
        for root in &roots {
            assert_eq!(tree(root), source_before);
        }
    }
    assert_ne!(fingerprints[0], fingerprints[1]);
    assert_eq!(fingerprints[0], fingerprints[2]);
    let persisted: Value = serde_json::from_slice(&fs::read(&server.config_path).unwrap()).unwrap();
    let profile = &persisted["models"][0]["execution_profile"];
    assert_eq!(profile["runtime_package"], installed["package"]);
    assert_eq!(profile["previous_runtime_package"], replacement["package"]);
    assert!(profile.get("command").is_none() && profile.get("runtime_bundle").is_none());
    for pid in worker_pids
        .iter()
        .copied()
        .chain(manager_pids.iter().map(|pid| u64::from(*pid)))
    {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    assert_eq!(digest(&model), model_before);
    assert_eq!(digest(&engine), engine_before);
    assert_eq!(digest(&manifest), manifest_before);
    assert_eq!(digest(&wav), audio_before);
    for file in &files {
        assert_eq!(
            digest(&original_root.join(file["path"].as_str().unwrap())),
            file["sha256"]
        );
    }
    json!({
        "package":installed["package"],"replacement_package":replacement["package"],"members":files.len(),"bytes":installed["reserved_bytes"],"bundle_digest":installed["bundle_digest"],
        "engine_sha256":engine_before,"model_sha256":model_before,"bundle_manifest_sha256":manifest_before,"wav_sha256":audio_before,
        "successful_transcriptions":calls,"worker_generations":worker_pids.len(),"manager_generations":manager_pids.len(),"fingerprint_changes_on_promotion_and_returns_after_rollback":true,
        "temporary_import_source_removed":true,"original_inputs_and_installed_payloads_unchanged":true,"owned_processes_exited":true
    })
}

#[tokio::test]
async fn managed_runtime_identity_promotion_and_rollback_preserve_inputs_policy_and_exact_cache_binding()
 {
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["cache_enabled"] = json!(true);
        config["models"] = json!([]);
    }));
    server.isolate_loader_environment();
    let source = tempfile::tempdir().unwrap();
    let worker = binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let original = digest(&worker);
    let bundle = one_file_source(source.path(), &worker);
    let input = source.path().join("input.json");
    // Import metadata must live outside the exact runtime closure.
    let metadata = tempfile::tempdir().unwrap();
    let input = metadata.path().join(input.file_name().unwrap());
    let mut request = import_spec(bundle);
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let first = success(import(&server, &input).await);
    request["provenance"]["build_revision"] = json!("second-declared-build-same-executable");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let second = success(import(&server, &input).await);
    assert_ne!(first["package"], second["package"]);
    assert_eq!(first["bundle_digest"], second["bundle_digest"]);
    fs::remove_file(source.path().join("engine")).unwrap();
    server.config["schema_version"] = json!(2);
    server.config["models"] = json!([{"id":"managed-runtime","model_file":server.directory.path().join("fixture weights.gguf"),
        "execution_profile":{"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":first["package"],
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--parallel","1","--ctx-size","256","--n-gpu-layers","0","--slot-save-path","{cache}"],"options":{},"placement":"unresolved"},
        "resident_bytes":64,"max_inflight":1,"operations":["completions"],"policy":"on-demand"}]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    let before_payloads = tree(&server.directory.path().join("runtime-packages"));
    server.start().await;
    let path = "models/managed-runtime/runtime";
    let initial = server.get(path).await;
    assert_eq!(initial["current"], first["package"]);
    assert_eq!(initial["previous"], Value::Null);
    assert_eq!(initial["supported"], true);
    for prefix in support::PREFIXES {
        let (status, view) = server
            .json(
                reqwest::Method::GET,
                &format!("{prefix}/juiced/runtime-packages"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["packages"].as_array().unwrap().len(), 2);
        assert!(
            view["packages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["phase"] == "installed" && item["compatibility"] == "unqualified")
        );
    }
    let unchanged = fs::read(&server.config_path).unwrap();
    for (change, code) in [
        (json!({"action":"rollback"}), "runtime_rollback_unavailable"),
        (
            json!({"action":"promote","package":first["package"]}),
            "runtime_revision_unchanged",
        ),
        (
            json!({"action":"promote","package":format!("package-{}", "0".repeat(64))}),
            "package_not_found",
        ),
    ] {
        let (status, error) = server
            .post(
                path,
                json!({"expected_revision":initial["revision"],"change":change}),
            )
            .await;
        assert_eq!(
            status,
            if code == "package_not_found" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            },
            "{error}"
        );
        assert_eq!(error["error"]["code"], code);
        assert_eq!(fs::read(&server.config_path).unwrap(), unchanged);
    }
    assert_eq!(
        server
            .post("load", json!({"model":"managed-runtime"}))
            .await
            .0,
        StatusCode::OK
    );
    complete(&server, "managed-runtime", "runtime-revision-session", 2).await;
    let before = server.get("juiced/context?model=managed-runtime").await;
    assert_eq!(
        before["applied"]["launch_plan"]["runtime_package"],
        first["package"]
    );
    let cache = server.get("juiced/cache").await;
    let (status, saved) = server.post("juiced/cache/save", json!({"model":"managed-runtime","slot":0,"session":"runtime-revision-session","expected_generation":cache["generation"],"maximum_bytes":4096})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, error) = server.post(path, json!({"expected_revision":initial["revision"],"change":{"action":"promote","package":second["package"]}})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "worker_in_use");
    assert_eq!(
        server
            .post("unload", json!({"model":"managed-runtime"}))
            .await
            .0,
        StatusCode::OK
    );
    let mut view = initial;
    for (index, prefix) in support::PREFIXES.into_iter().enumerate() {
        let target = if index % 2 == 0 { &second } else { &first };
        let old = view["current"].clone();
        let (status, changed) = server.json(reqwest::Method::POST, &format!("{prefix}/{path}"), Some(json!({"expected_revision":view["revision"],"change":{"action":"promote","package":target["package"]}})), server.admin_key()).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(changed["current"], target["package"]);
        assert_eq!(changed["previous"], old);
        assert_eq!(changed["policy"], "on-demand");
        assert_eq!(changed["worker_in_use"], false);
        let (status, inspected) = server
            .json(
                reqwest::Method::GET,
                &format!("{prefix}/{path}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(changed, inspected);
        view = changed;
    }
    let (status, promoted) = server.post(path, json!({"expected_revision":view["revision"],"change":{"action":"promote","package":second["package"]}})).await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    let (status, stale) = server
        .post(
            path,
            json!({"expected_revision":view["revision"],"change":{"action":"rollback"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "revision_conflict");
    server.restart().await;
    assert_eq!(server.get(path).await["current"], second["package"]);
    let (status, mismatch) = server.post("juiced/cache/restore", json!({"model":"managed-runtime","slot":0,"session":"runtime-revision-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "snapshot_identity_mismatch");
    let changed = server.get("juiced/context?model=managed-runtime").await;
    assert_ne!(
        before["applied"]["launch_plan"]["semantic_fingerprint"],
        changed["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"managed-runtime"}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, rolled) = server
        .post(
            path,
            json!({"expected_revision":promoted["revision"],"change":{"action":"rollback"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rolled}");
    assert_eq!(rolled["current"], first["package"]);
    assert_eq!(rolled["previous"], second["package"]);
    let (status, edited) = server
        .post(
            "models/managed-runtime/options",
            json!({"expected_revision":rolled["revision"],"options":{"context_tokens":256}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    let retained = server.get(path).await;
    assert_eq!(retained["current"], first["package"]);
    assert_eq!(retained["previous"], second["package"]);
    server.restart().await;
    let persisted: Value = serde_json::from_slice(&fs::read(&server.config_path).unwrap()).unwrap();
    let profile = &persisted["models"][0]["execution_profile"];
    assert_eq!(profile["runtime_package"], first["package"]);
    assert_eq!(profile["previous_runtime_package"], second["package"]);
    assert!(profile.get("command").is_none() && profile.get("runtime_bundle").is_none());
    assert_eq!(persisted["models"][0]["policy"], "on-demand");
    let (status, restored) = server.post("juiced/cache/restore", json!({"model":"managed-runtime","slot":0,"session":"runtime-revision-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    complete(&server, "managed-runtime", "runtime-revision-session", 2).await;
    let resumed = server.get("juiced/context?model=managed-runtime").await;
    assert_eq!(
        before["applied"]["launch_plan"]["semantic_fingerprint"],
        resumed["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_ne!(before["applied"]["pid"], resumed["applied"]["pid"]);
    server.stop();
    assert_eq!(
        tree(&server.directory.path().join("runtime-packages")),
        before_payloads
    );
    assert_eq!(digest(&worker), original);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shared_installed_runtime_keeps_other_model_live_through_promotion_unregister_and_cache_restart()
 {
    let mut receipt = shared_runtime_pipeline(None).await;
    receipt["scope"] = json!(
        "Synthetic subprocesses sharing an installed llama-cpp package. No model inference or quality qualification."
    );
    println!("shared-synthetic-runtime-receipt {receipt}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real CPU llama-server/GGUF assets; shared installed runtime and model/cache isolation, not output quality"]
async fn tiny_gguf_shared_runtime_models_promote_unregister_and_restore_the_survivor() {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let mut receipt = shared_runtime_pipeline(Some((engine, model))).await;
    receipt["scope"] = json!(
        "Two actual selected CPU llama.cpp workers sharing installed package and model bytes. Cross-model cache refusal, independent promotion/unregister and survivor restart/restore. No output quality, hardware/ABI qualification, online acquisition or reclamation claim."
    );
    println!("shared-real-runtime-receipt {receipt}");
}

#[cfg(target_os = "linux")]
async fn shared_runtime_pipeline(real: Option<(PathBuf, PathBuf)>) -> Value {
    const ALPHA: &str = "shared-runtime-alpha";
    const BETA: &str = "shared-runtime-beta";
    const SESSION: &str = "shared-session-is-not-shared-model-state";
    let actual = real.is_some();
    let resident = if actual { 512u64 * 1024 * 1024 } else { 64 };
    let mut server = InstalledServer(Server::prepare(true, |config| {
        config["models"] = json!([]);
        config["cache_enabled"] = json!(true);
        config["resident_budget_bytes"] = json!(resident * 4);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
    }));
    server.isolate_loader_environment();
    let (worker, model) = real.unwrap_or_else(|| {
        (
            binary("juiced-test-worker", "JUICED_TEST_WORKER"),
            server.directory.path().join("fixture weights.gguf"),
        )
    });
    let worker_before = digest(&worker);
    let model_before = digest(&model);
    let source = tempfile::tempdir().unwrap();
    let input = server.directory.path().join("import.json");
    let bundle = if actual {
        support::llama::flat_cpu_bundle(&worker, source.path()).1
    } else {
        one_file_source(source.path(), &worker)
    };
    let mut request = import_spec(bundle);
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let first = success(import(&server, &input).await);
    request["provenance"]["build_revision"] = json!("shared-model-replacement");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let second = success(import(&server, &input).await);
    fs::remove_dir_all(source.path()).unwrap();
    assert!(!source.path().exists());
    let package_root = server.directory.path().join("runtime-packages");
    let packages_before = tree(&package_root);
    server.config["schema_version"] = json!(2);
    server.config["models"] = json!([ALPHA, BETA].map(|id| json!({
        "id":id,"model_file":model,
        "execution_profile":{"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":first["package"],
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--parallel","1","--ctx-size","256","--threads","2","--slots","--n-gpu-layers","0","--slot-save-path","{cache}"],"options":{},"placement":"unresolved"},
        "resident_bytes":resident,"max_inflight":1,"operations":["completions"],"policy":"on-demand"
    })));
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    server.start().await;
    let (alpha, beta) = tokio::join!(
        server.post("load", json!({"model":ALPHA})),
        server.post("load", json!({"model":BETA}))
    );
    assert_eq!(alpha.0, StatusCode::OK, "{}", alpha.1);
    assert_eq!(beta.0, StatusCode::OK, "{}", beta.1);
    assert_eq!(alpha.1["runtime_package"], first["package"]);
    assert_eq!(beta.1["runtime_package"], first["package"]);
    assert_eq!(package_bindings(&server, &first["package"]).await, 2);
    assert_eq!(package_bindings(&server, &second["package"]).await, 0);
    let alpha_pid = alpha.1["pid"].as_u64().unwrap();
    let beta_pid = beta.1["pid"].as_u64().unwrap();
    assert_ne!(alpha_pid, beta_pid);
    tokio::join!(
        complete(&server, ALPHA, SESSION, 2),
        complete(&server, BETA, SESSION, 2)
    );
    let beta_context = server.get(&format!("juiced/context?model={BETA}")).await;
    let cache = server.get("juiced/cache").await;
    let (status, saved) = server.post("juiced/cache/save",json!({"model":BETA,"slot":0,"session":SESSION,"expected_generation":cache["generation"],"maximum_bytes":2*1024*1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    let (status, mismatch) = server
        .post(
            "juiced/cache/restore",
            json!({"model":ALPHA,"slot":0,"session":SESSION,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "snapshot_identity_mismatch");
    let (status, unloaded) = server.post("unload", json!({"model":ALPHA})).await;
    assert_eq!(status, StatusCode::OK, "{unloaded}");
    assert!(!Path::new(&format!("/proc/{alpha_pid}")).exists());
    let path = format!("models/{ALPHA}/runtime");
    let view = server.get(&path).await;
    let (status, promoted) = server.post(&path,json!({"expected_revision":view["revision"],"change":{"action":"promote","package":second["package"]}})).await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    assert_eq!(promoted["current"], second["package"]);
    assert_eq!(promoted["previous"], first["package"]);
    assert_eq!(package_bindings(&server, &first["package"]).await, 2);
    assert_eq!(package_bindings(&server, &second["package"]).await, 1);
    assert_eq!(server.model(BETA).await["pid"], beta_pid);
    let still_live = server.get(&format!("juiced/context?model={BETA}")).await;
    for field in ["pid", "launch", "launch_plan", "execution_fingerprint"] {
        assert_eq!(still_live["applied"][field], beta_context["applied"][field]);
    }
    for field in ["process_generation", "closure_digest", "bundle_digest"] {
        assert_eq!(
            still_live["applied"]["runtime_identity"][field],
            beta_context["applied"]["runtime_identity"][field]
        );
    }
    complete(&server, BETA, SESSION, 2).await;
    let (status, disabled) = server
        .post(
            "juiced/policy",
            json!({"model":ALPHA,"policy":"disabled","expected_revision":promoted["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let (status, removed) = server
        .post(
            "delete",
            json!({"model":ALPHA,"expected_revision":disabled["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(server.model(BETA).await["pid"], beta_pid);
    assert_eq!(package_bindings(&server, &first["package"]).await, 1);
    assert_eq!(package_bindings(&server, &second["package"]).await, 0);
    assert_eq!(tree(&package_root), packages_before);
    complete(&server, BETA, SESSION, 2).await;
    server.restart().await;
    assert!(!Path::new(&format!("/proc/{beta_pid}")).exists());
    assert_eq!(
        server.get("juiced/catalog").await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        server.get(&format!("models/{BETA}/runtime")).await["current"],
        first["package"]
    );
    let (status, restored) = server
        .post(
            "juiced/cache/restore",
            json!({"model":BETA,"slot":0,"session":SESSION,"snapshot":saved["snapshot"]["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    assert_eq!(package_bindings(&server, &first["package"]).await, 1);
    assert_eq!(package_bindings(&server, &second["package"]).await, 0);
    let resumed = server.get(&format!("juiced/context?model={BETA}")).await;
    assert_eq!(
        resumed["applied"]["launch_plan"]["semantic_fingerprint"],
        beta_context["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    let resumed_pid = resumed["applied"]["pid"].as_u64().unwrap();
    assert_ne!(resumed_pid, beta_pid);
    complete(&server, BETA, SESSION, 2).await;
    server.stop();
    for pid in [alpha_pid, beta_pid, resumed_pid] {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    assert_eq!(tree(&package_root), packages_before);
    assert_eq!(digest(&worker), worker_before);
    assert_eq!(digest(&model), model_before);
    json!({"first_package":first["package"],"replacement_package":second["package"],"engine_sha256":worker_before,"model_sha256":model_before,"members":request["bundle"]["files"].as_array().unwrap().len(),"payload_bytes":first["reserved_bytes"],"cross_model_cache_refused":true,"other_model_pid_launch_and_closure_preserved":true,"saved_bytes":saved["engine"]["n_written"],"restored_bytes":restored["engine"]["n_read"],"survivor_fingerprint_preserved_after_restart":true,"temporary_source_removed":true,"installed_payload_and_original_inputs_unchanged":true,"owned_workers_exited":true})
}

#[tokio::test]
async fn first_managed_selection_is_explicit_preserves_disabled_registration_and_has_no_managed_predecessor()
 {
    let mut server = InstalledServer(Server::prepare(true, |config| config["models"] = json!([])));
    let source = tempfile::tempdir().unwrap();
    let worker = binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let bundle = one_file_source(source.path(), &worker);
    let metadata = tempfile::tempdir().unwrap();
    let input = metadata.path().join("input.json");
    fs::write(&input, serde_json::to_vec(&import_spec(bundle)).unwrap()).unwrap();
    let installed = success(import(&server, &input).await);
    server.config["models"] = json!([{"id":"first-runtime","model_file":server.directory.path().join("fixture weights.gguf"),"provider":"llama-cpp","command":worker,"args":["--model","{model}","--port","{port}"],"resident_bytes":64,"max_inflight":1,"operations":["completions"],"policy":"disabled"}]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    server.start().await;
    let path = "models/first-runtime/runtime";
    let before = server.get(path).await;
    assert_eq!(before["current"], Value::Null);
    let (status, selected) = server.post(path, json!({"expected_revision":before["revision"],"change":{"action":"promote","package":installed["package"]}})).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(selected["current"], installed["package"]);
    assert_eq!(selected["previous"], Value::Null);
    assert_eq!(selected["policy"], "disabled");
    assert_eq!(selected["worker_in_use"], false);
    server.restart().await;
    assert_eq!(server.get(path).await["current"], installed["package"]);
    let catalog = server.get("juiced/catalog").await;
    let model = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "first-runtime")
        .unwrap();
    assert_eq!(model["loaded"], false);
    assert_eq!(model["policy"], "disabled");
    server.stop();
}

#[tokio::test]
async fn foreign_declared_runtime_target_refuses_all_prefix_selection_and_startup_without_mutating_inputs()
 {
    let mut server = InstalledServer(Server::prepare(true, |config| config["models"] = json!([])));
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("engine"), b"unqualified foreign fixture").unwrap();
    let bundle = json!({"schema_version":1,"implementation_id":"llama-cpp","root":source.path(),"entrypoint":"engine","library_directories":[],"files":[{"path":"engine","sha256":digest(&source.path().join("engine")),"role":"executable"}]});
    let metadata = tempfile::tempdir().unwrap();
    let input = metadata.path().join("input.json");
    let mut request = import_spec(bundle);
    request["declared_target"]["operating_system"] = json!("foreign-target");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let installed = success(import(&server, &input).await);
    server.config["models"] = json!([{"id":"target-guard","model_file":server.directory.path().join("fixture weights.gguf"),"provider":"llama-cpp","command":binary("juiced-test-worker", "JUICED_TEST_WORKER"),"args":["--model","{model}","--port","{port}"],"resident_bytes":64,"max_inflight":1,"operations":["completions"],"policy":"disabled"}]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    let configured = fs::read(&server.config_path).unwrap();
    let retained = tree(&server.directory.path().join("runtime-packages"));
    let source_before = tree(source.path());
    server.start().await;
    let inventory = server.get("juiced/runtime-packages").await;
    assert_eq!(inventory["packages"][0]["declared_host_match"], false);
    assert_eq!(inventory["packages"][0]["compatibility"], "unqualified");
    let before = server.get("models/target-guard/runtime").await;
    for prefix in support::PREFIXES {
        let (status, error) = server.json(reqwest::Method::POST, &format!("{prefix}/models/target-guard/runtime"), Some(json!({"expected_revision":before["revision"],"change":{"action":"promote","package":installed["package"]}})), server.admin_key()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "package_target_mismatch");
        assert_eq!(server.get("models/target-guard/runtime").await, before);
        assert_eq!(fs::read(&server.config_path).unwrap(), configured);
        assert_eq!(
            tree(&server.directory.path().join("runtime-packages")),
            retained
        );
    }
    server.stop();
    let mut registration = support::canonical_model(server.config["models"][0].clone());
    let profile = registration["execution_profile"].as_object_mut().unwrap();
    profile.remove("command");
    profile.insert("runtime_package".into(), installed["package"].clone());
    server.config["schema_version"] = json!(2);
    server.config["models"] = json!([registration]);
    fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    let invalid = fs::read(&server.config_path).unwrap();
    let rejected = support::rejected_manager(&server.config_path).await;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("package_target_mismatch"));
    assert_eq!(fs::read(&server.config_path).unwrap(), invalid);
    assert_eq!(
        tree(&server.directory.path().join("runtime-packages")),
        retained
    );
    assert_eq!(tree(source.path()), source_before);
}
