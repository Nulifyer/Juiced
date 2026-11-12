mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use support::{
    Server, binary, canonical_model,
    speech::{self, Part},
};

fn prepare_selected(adapter: &str) -> Server {
    Server::prepare(true, |config| {
        let root = Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap();
        let command = root.join(format!(
            "{}{}",
            if adapter == "llama-cpp" {
                "llama-server"
            } else {
                "whisper-server"
            },
            std::env::consts::EXE_SUFFIX
        ));
        let fixture = binary("juiced-test-worker", "JUICED_TEST_WORKER");
        std::fs::copy(&fixture, &command).unwrap();
        std::fs::set_permissions(&command, std::fs::metadata(&fixture).unwrap().permissions())
            .unwrap();
        let model = &mut config["models"][0];
        model["command"] = json!(command);
        model["provider"] = json!(adapter);
        model["max_inflight"] = json!(1);
        model["args"] = json!(["-m", "{model}", "--port", "{port}", "--host", "127.0.0.1"]);
        if adapter == "llama-cpp" {
            model["operations"] = json!(["completions"]);
            model["options"] =
                json!({"parallel_slots":1,"context_tokens":512,"threads":2,"gpu_layers":0});
        } else {
            model["operations"] = json!(["audio/transcriptions"]);
            model["args"].as_array_mut().unwrap().extend([
                json!("--threads"),
                json!("2"),
                json!("--no-gpu"),
            ]);
        }
    })
}
fn isolate_user_config(server: &mut Server) -> (PathBuf, PathBuf) {
    let home = server.directory.path().join("isolated-home");
    let xdg = server.directory.path().join("isolated-xdg");
    std::fs::create_dir_all(home.join(".config").join("llama.cpp")).unwrap();
    std::fs::create_dir_all(xdg.join("llama.cpp")).unwrap();
    server.set_environment("HOME", &home);
    server.set_environment("XDG_CONFIG_HOME", &xdg);
    (home, xdg)
}
async fn assert_rejected_arguments(server: &Server, mut spec: Value, args: Value) {
    let bytes = std::fs::read(&server.config_path).unwrap();
    let catalog = server.get("juiced/catalog").await;
    spec["policy"] = json!("disabled");
    spec["revision"] = json!(0);
    spec["args"] = args;
    for nested in [false, true] {
        spec["id"] = json!(if nested {
            "invalid-nested-arguments"
        } else {
            "invalid-legacy-arguments"
        });
        let body = if nested {
            canonical_model(spec.clone())
        } else {
            spec.clone()
        };
        let (status, error) = server.post("models/register", body).await;
        assert_eq!(
            status,
            if nested {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::BAD_REQUEST
            },
            "{error}"
        );
        assert!(error["error"]["code"].is_string());
        assert!(!error.to_string().contains("synthetic-private-argument"));
        assert_eq!(std::fs::read(&server.config_path).unwrap(), bytes);
        assert_eq!(server.get("juiced/catalog").await, catalog);
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
    }
}

#[tokio::test]
async fn selected_llama_owned_bindings_accept_production_aliases_and_reject_unsafe_forms_before_spawn()
 {
    let mut server = prepare_selected("llama-cpp");
    isolate_user_config(&mut server);
    let args = json!([
        "-m={model}",
        "--port={port}",
        "--slot_save_path={cache}",
        "--parallel=1",
        "--ctx-size=512",
        "--threads=2",
        "--gpu-layers=0",
        "--slots"
    ]);
    server.config["models"][0]["args"] = args;
    std::fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    for name in [
        "LLAMA_ARG_MODEL",
        "LLAMA_ARG_PORT",
        "GGML_BACKEND_PATH",
        "HF_TOKEN",
        "HTTP_PROXY",
    ] {
        server.set_environment(name, "synthetic-private-argument");
    }
    server.start().await;
    let bytes = std::fs::read(&server.config_path).unwrap();
    let (status, loaded) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["implementation_id"], "llama-cpp");
    let (status, response) = server
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Selected argument pipeline"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["model"], "fixture.one");
    #[cfg(target_os = "linux")]
    {
        let environment =
            std::fs::read(format!("/proc/{}/environ", loaded["pid"].as_u64().unwrap())).unwrap();
        for name in [
            b"LLAMA_ARG_MODEL=".as_slice(),
            b"LLAMA_ARG_PORT=",
            b"GGML_BACKEND_PATH=",
            b"HF_TOKEN=",
            b"HTTP_PROXY=",
        ] {
            assert!(
                !environment
                    .split(|byte| *byte == 0)
                    .any(|value| value.starts_with(name))
            );
        }
    }
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), bytes);
    let base = json!([
        "--model",
        "{model}",
        "--port",
        "{port}",
        "--host",
        "127.0.0.1"
    ]);
    let additions = [
        json!(["-m={model}"]),
        json!(["--port={port}"]),
        json!(["--host=127.0.0.1"]),
        json!(["--slot-save-path", "{cache}", "--slot_save_path={cache}"]),
        json!(["--model-url", "https://invalid.example/model"]),
        json!(["--hf-repo", "unbound/repo"]),
        json!(["--mcp-servers-json", "{}"]),
        json!(["--api_key=synthetic-private-argument"]),
        json!(["--lora", "unbound-companion.gguf"]),
        json!(["--mmproj", "unbound-companion.gguf"]),
        json!(["--grammar-file", "unbound-grammar.txt"]),
        json!(["--models-dir", "unbound-directory"]),
        json!(["--gpt-oss-20b-default"]),
        json!(["--sleep-idle-seconds", "1"]),
        json!(["--slots=true"]),
        json!(["--threads"]),
        json!(["--", "unbound-positional"]),
        json!(["unbound-positional"]),
    ];
    for extra in additions {
        let mut args = base.clone();
        args.as_array_mut()
            .unwrap()
            .extend(extra.as_array().unwrap().iter().cloned());
        assert_rejected_arguments(&server, server.config["models"][0].clone(), args).await;
    }
    for args in [
        json!(["--model", "fixed-model.gguf", "--port", "{port}"]),
        json!(["--model", "prefix-{model}", "--port", "{port}"]),
        json!(["--model", "{model}", "--port", "1234"]),
        json!(["--model", "{model}", "--port="]),
        json!(["--model", "{model}", "--port"]),
        json!([
            "--model", "{model}", "--port", "{port}", "--host", "0.0.0.0"
        ]),
        json!([
            "--model",
            "{model}",
            "--port",
            "{port}",
            "--slot-save-path",
            "unbound-cache"
        ]),
    ] {
        assert_rejected_arguments(&server, server.config["models"][0].clone(), args).await;
    }
}

#[tokio::test]
async fn selected_whisper_production_arguments_reject_conversion_authority_and_unbound_paths() {
    let mut server = prepare_selected("whisper-cpp");
    isolate_user_config(&mut server);
    server.start().await;
    let audio = speech::audio();
    let response = speech::send(
        &server,
        "/v1/audio/transcriptions",
        &[
            Part::Text("model", "fixture.one"),
            Part::File("file", "fixture.wav", "audio/wav", &audio),
        ],
        server.read_key(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    let base = server.config["models"][0]["args"].clone();
    for extra in [
        json!(["--convert"]),
        json!(["--translate"]),
        json!(["-tr"]),
        json!(["--public", "unbound-public"]),
        json!(["--request-path", "/unbound"]),
        json!(["--inference-path", "/unbound"]),
        json!(["--tmp-dir", "unbound-temp"]),
        json!(["--vad-model", "unbound-vad"]),
        json!(["--model-url", "https://invalid.example/model"]),
        json!(["--mcp-servers-json", "{}"]),
        json!(["--port={port}"]),
    ] {
        let mut args = base.clone();
        args.as_array_mut()
            .unwrap()
            .extend(extra.as_array().unwrap().iter().cloned());
        assert_rejected_arguments(&server, server.config["models"][0].clone(), args).await;
    }
    assert_rejected_arguments(
        &server,
        server.config["models"][0].clone(),
        json!(["-m", "{model}", "--port", "{port}"]),
    )
    .await;
}

#[tokio::test]
async fn opaque_http_worker_does_not_gain_native_cache_capability_from_slot_save_argument_text() {
    let mut server = Server::prepare(true, |config| {
        config["cache_enabled"] = json!(true);
        let root = Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap();
        let command = root.join(format!(
            "custom-http-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::copy(binary("juiced-test-worker", "JUICED_TEST_WORKER"), &command).unwrap();
        config["models"][0]["command"] = json!(command);
        config["models"][0]["max_inflight"] = json!(1);
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--slot-save-path"), json!("{cache}")]);
    });
    isolate_user_config(&mut server);
    server.start().await;
    let response = server
        .client
        .post(format!("{}/v1/completions", server.base))
        .bearer_auth(server.admin_key().unwrap())
        .header("x-juiced-session", "opaque-cache")
        .json(&json!({"model":"fixture.one","prompt":"Opaque HTTP pipeline"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    assert_eq!(
        server.model("fixture.one").await["implementation_id"],
        Value::Null
    );
    let before = server.get("juiced/cache").await;
    let(status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"opaque-cache","expected_generation":before["generation"],"maximum_bytes":1024})).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{error}");
    assert_eq!(error["error"]["code"], "cache_profile_unsupported");
    assert!(!server.event("fixture.one", "slot-save", "started"));
    assert_eq!(server.get("juiced/cache").await, before);
}

#[tokio::test]
async fn selected_llama_user_configuration_is_checked_without_loading_or_reading_its_contents() {
    let mut server = prepare_selected("llama-cpp");
    server.config["cache_enabled"] = json!(true);
    server.config["models"][0]["args"]
        .as_array_mut()
        .unwrap()
        .extend([json!("--slot-save-path"), json!("{cache}")]);
    std::fs::write(
        &server.config_path,
        serde_json::to_vec(&server.config).unwrap(),
    )
    .unwrap();
    let (home, xdg) = isolate_user_config(&mut server);
    let directory = xdg.join("llama.cpp");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("config.ini");
    let private = b"synthetic-private-runtime-config-not-for-reading";
    std::fs::write(&path, private).unwrap();
    server.start().await;
    let saved = std::fs::read(&server.config_path).unwrap();
    let (status, error) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "unbound_runtime_config");
    assert!(
        !error
            .to_string()
            .contains(std::str::from_utf8(private).unwrap())
    );
    let model = server.model("fixture.one").await;
    assert!(!model["loaded"].as_bool().unwrap());
    assert!(model["pid"].is_null());
    assert!(model["process_lifecycle"].is_null());
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), saved);
    assert_eq!(std::fs::read(&path).unwrap(), private);
    std::fs::write(&path, b"").unwrap();
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    std::fs::remove_file(&path).unwrap();
    #[cfg(target_os = "linux")]
    {
        let response = server
            .client
            .post(format!("{}/v1/completions", server.base))
            .bearer_auth(server.admin_key().unwrap())
            .header("x-juiced-session", "configuration-guard")
            .json(&json!({"model":"fixture.one","prompt":"Absent runtime configuration pipeline"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
        let pid = server.model("fixture.one").await["pid"].clone();
        let before = server.get("juiced/cache").await;
        let cache_root = server.config_path.parent().unwrap().join("cache");
        let files = || {
            std::fs::read_dir(&cache_root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let original_files = files();
        std::fs::write(&path, private).unwrap();
        for remove in [false, true] {
            if remove {
                std::fs::remove_file(&path).unwrap();
            }
            let(status,error)=server.post("juiced/cache/save",json!({"model":"fixture.one","slot":0,"session":"configuration-guard","expected_generation":before["generation"],"maximum_bytes":1024})).await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["error"]["code"], "unbound_runtime_config");
            assert_eq!(server.get("juiced/cache").await, before);
            assert_eq!(files(), original_files);
            assert_eq!(server.model("fixture.one").await["pid"], pid);
        }
        assert_eq!(
            server
                .post("unload", json!({"model":"fixture.one"}))
                .await
                .0,
            StatusCode::OK
        );
    }
    std::fs::remove_dir(&directory).unwrap();
    std::fs::write(&directory, b"inaccessible-parent-fixture").unwrap();
    let (status, error) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "unbound_runtime_config");
    assert!(server.model("fixture.one").await["pid"].is_null());
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), saved);
    server.stop();
    std::fs::remove_file(&directory).unwrap();
    server.remove_environment("XDG_CONFIG_HOME");
    let fallback = home.join(".config").join("llama.cpp");
    std::fs::create_dir_all(&fallback).unwrap();
    let fallback = fallback.join("config.ini");
    std::fs::write(&fallback, private).unwrap();
    server.start().await;
    let (status, error) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "unbound_runtime_config");
    assert!(server.model("fixture.one").await["pid"].is_null());
    std::fs::write(&fallback, b"").unwrap();
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), saved);
}
