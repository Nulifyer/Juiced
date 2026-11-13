mod support;

use reqwest::StatusCode;
use serde_json::json;
use std::path::PathBuf;
use support::Server;

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; checks real data flow, not linguistic quality"]
async fn tiny_gguf_load_infer_unload_restart_pipeline() {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER")
            .expect("set JUICED_TEST_LLAMA_SERVER to an installed llama-server"),
    );
    let model = PathBuf::from(
        std::env::var_os("JUICED_TEST_GGUF")
            .expect("set JUICED_TEST_GGUF to an existing tiny GGUF"),
    );
    assert!(engine.is_file(), "engine executable is unavailable");
    assert!(model.is_file(), "GGUF weights are unavailable");
    let mut server = Server::configured(false, |config| {
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([{
            "id":"tiny-real-model", "model_file":model, "command":engine,"provider":"llama-cpp",
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--slots","--n-gpu-layers","0"],
            "resident_bytes":512u64 * 1024 * 1024, "max_inflight":1,
            "operations":["completions"], "policy":"on-demand",
        }]);
    }).await;
    for _ in 0..2 {
        let (status, loaded) = server
            .post("load", json!({"model":"tiny-real-model"}))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(loaded["loaded"], true);
        let context = server.get("juiced/context?model=tiny-real-model").await;
        assert!(
            context["declared"]["training_context_tokens"]
                .as_u64()
                .is_some_and(|tokens| tokens > 0)
        );
        assert!(context["declared"]["architecture"].is_string());
        assert_eq!(context["configured"]["context_tokens"], 256);
        assert_eq!(context["applied"]["launch"]["context_tokens"], 256);
        assert_eq!(context["observed"]["slots"].as_array().unwrap().len(), 1);
        assert!(
            context["observed"]["slots"][0]["context_tokens"]
                .as_u64()
                .is_some_and(|tokens| tokens > 0)
        );
        assert_eq!(context["observation_error"], serde_json::Value::Null);
        let (status, completion) = server.post("completions", json!({"model":"tiny-real-model", "prompt":"Once upon a time", "max_tokens":8, "temperature":0, "stream":false})).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            completion["choices"]
                .as_array()
                .is_some_and(|choices| !choices.is_empty())
        );
        assert!(
            completion["usage"]["completion_tokens"]
                .as_u64()
                .is_some_and(|tokens| tokens > 0)
        );
        assert_eq!(
            server
                .post("unload", json!({"model":"tiny-real-model"}))
                .await
                .1["loaded"],
            false
        );
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
        server.restart().await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF with native slot save/restore support"]
async fn tiny_gguf_cache_save_restart_restore_and_continue_pipeline() {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    assert!(engine.is_file());
    assert!(model.is_file());
    let mut server = Server::configured(true, |config| {
        config["cache_enabled"] = json!(true);
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([{
            "id":"tiny-real-model", "model_file":model, "command":engine,"provider":"llama-cpp",
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--slots","--n-gpu-layers","0","--slot-save-path","{cache}"],
            "resident_bytes":512u64 * 1024 * 1024, "max_inflight":1,
            "operations":["completions"], "policy":"on-demand",
        }]);
    }).await;
    let session = "tiny-engine-cache-pipeline";
    let response=server.client.post(format!("{}/v1/completions",server.base)).bearer_auth(server.admin_key().unwrap()).header("x-juiced-session",session).json(&json!({"model":"tiny-real-model","prompt":"Once upon a time","max_tokens":8,"temperature":0,"stream":false,"cache_prompt":true,"id_slot":0})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let completion: serde_json::Value = response.json().await.unwrap();
    assert!(completion["usage"]["completion_tokens"].as_u64().unwrap() > 0);
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    let cache = server.get("juiced/cache").await;
    let (status,saved)=server.post("juiced/cache/save",json!({"model":"tiny-real-model","slot":0,"session":session,"expected_generation":cache["generation"],"maximum_bytes":16*1024*1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_saved"].as_u64().unwrap() > 0);
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    assert!(saved["snapshot"]["bytes"].as_u64().unwrap() > 0);
    let (status, _) = server
        .post("unload", json!({"model":"tiny-real-model"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    server.restart().await;
    let (status,restored)=server.post("juiced/cache/restore",json!({"model":"tiny-real-model","slot":0,"session":session,"snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(restored["engine"]["n_restored"].as_u64().unwrap() > 0);
    assert!(restored["engine"]["n_read"].as_u64().unwrap() > 0);
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    let response=server.client.post(format!("{}/v1/completions",server.base)).bearer_auth(server.admin_key().unwrap()).header("x-juiced-session",session).json(&json!({"model":"tiny-real-model","prompt":"Once upon a time there was","max_tokens":4,"temperature":0,"stream":false,"cache_prompt":true,"id_slot":0})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let continuation: serde_json::Value = response.json().await.unwrap();
    assert!(continuation["usage"]["completion_tokens"].as_u64().unwrap() > 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; selected actual five-slot CPU flow"]
async fn tiny_gguf_five_observed_slots_queue_clients_without_changing_context() {
    const MODEL: &str = "tiny-real-five-slots";
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let mut server=Server::configured(true,|config| {
        config["resident_budget_bytes"]=json!(1024u64*1024*1024);config["startup_timeout_ms"]=json!(30000);config["request_timeout_ms"]=json!(30000);
        config["models"]=json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","2560","--parallel","5","--threads","2","--slots","--n-gpu-layers","0"],"resident_bytes":512u64*1024*1024,"max_inflight":5,"operations":["completions"],"policy":"on-demand"}]);
    }).await;
    let (status, loaded) = server.post("load", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let before = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_eq!(before["applied"]["launch"]["threads"], 2);
    let receipt = support::llama::five_slot_pipeline(
        &mut server,
        MODEL,
        loaded,
        before,
        false,
        &engine,
        &model,
    )
    .await;
    println!("real-five-slots-receipt {receipt}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real GGUF assets; selected CPU vector transport and restart, not embedding quality"]
async fn tiny_gguf_embedding_batches_restart_without_mutating_inputs() {
    use serde_json::Value;
    use support::{PREFIXES, filesystem::digest};
    const MODEL: &str = "tiny-real-embeddings";
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let engine_sha = digest(&engine);
    let model_sha = digest(&model);
    let mut server = Server::configured(true, |config| {
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--threads","2","--slots","--n-gpu-layers","0","--embedding","--pooling","mean"],"resident_bytes":512u64 * 1024 * 1024,"max_inflight":1,"operations":["embeddings"],"policy":"on-demand"}]);
    }).await;
    let original = std::fs::read(&server.config_path).unwrap();
    let mut dimensions = None;
    let mut worker_pids = Vec::new();
    let mut batches = 0;
    let mut gateway_vectors = 0;
    let mut gateway_requests = 0;
    for _ in 0..2 {
        let (status, loaded) = server.post("load", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
        let pid = loaded["pid"].as_u64().unwrap();
        assert!(!worker_pids.contains(&pid));
        worker_pids.push(pid);
        for prefix in PREFIXES {
            let url = format!("{}{prefix}/embeddings", server.base);
            let response = server
                .client
                .post(&url)
                .json(&json!({"model":MODEL,"input":"unauthenticated"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let response = server.client.post(&url).bearer_auth(server.read_key().unwrap()).json(&json!({"model":MODEL,"input":["Once upon a time","A separate input"],"encoding_format":"float"})).send().await.unwrap();
            let status = response.status();
            let body: Value = response.json().await.unwrap();
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["model"], MODEL);
            assert_eq!(body["object"], "list");
            let rows = body["data"].as_array().unwrap();
            assert_eq!(rows.len(), 2);
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(row["index"], index);
                assert_eq!(row["object"], "embedding");
                let vector = row["embedding"].as_array().unwrap();
                assert!(!vector.is_empty() && vector.len() <= 65536);
                assert!(
                    vector
                        .iter()
                        .all(|value| value.as_f64().is_some_and(f64::is_finite))
                );
                assert_eq!(*dimensions.get_or_insert(vector.len()), vector.len());
            }
            assert!(
                body["usage"]["prompt_tokens"]
                    .as_u64()
                    .is_some_and(|tokens| tokens > 0)
            );
            batches += 1;
        }
        for (path, body, field, batched) in [
            (
                "/api/embed",
                json!({"model":MODEL,"input":["First vector","Second vector"]}),
                "embeddings",
                true,
            ),
            (
                "/api/embeddings",
                json!({"model":MODEL,"prompt":"One vector"}),
                "embedding",
                false,
            ),
            (
                "/mcp",
                json!({"jsonrpc":"2.0","id":"real-embedding","method":"tools/call","params":{"name":"lemonade_embed","arguments":{"model":MODEL,"input":"One vector"}}}),
                "",
                false,
            ),
        ] {
            let (status, output) = server
                .json(reqwest::Method::POST, path, Some(body), server.read_key())
                .await;
            assert_eq!(status, StatusCode::OK, "{path}: {output}");
            let vectors = if path == "/mcp" {
                assert_eq!(output["id"], "real-embedding");
                assert_eq!(output["result"]["isError"], false, "{output}");
                assert_eq!(output["result"]["structuredContent"]["model"], MODEL);
                vec![&output["result"]["structuredContent"]["data"][0]["embedding"]]
            } else {
                if batched {
                    assert_eq!(output["model"], MODEL);
                    let rows = output[field].as_array().unwrap();
                    assert_eq!(rows.len(), 2);
                    rows.iter().collect::<Vec<_>>()
                } else {
                    vec![&output[field]]
                }
            };
            for vector in vectors {
                let vector = vector.as_array().unwrap();
                assert_eq!(vector.len(), dimensions.unwrap());
                assert!(
                    vector
                        .iter()
                        .all(|value| value.as_f64().is_some_and(f64::is_finite))
                );
                gateway_vectors += 1;
            }
            gateway_requests += 1;
        }
        let (status, invalid) = server
            .post(
                "embeddings",
                json!({"model":MODEL,"input":{"invalid":"input"}}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
        assert_eq!(server.model(MODEL).await["pid"], pid);
        let (status, single) = server
            .post(
                "embeddings",
                json!({"model":MODEL,"input":"Single input","encoding_format":"float"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{single}");
        assert_eq!(single["model"], MODEL);
        assert_eq!(single["data"].as_array().unwrap().len(), 1);
        assert_eq!(
            single["data"][0]["embedding"].as_array().unwrap().len(),
            dimensions.unwrap()
        );
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
        let (status, unloaded) = server.post("unload", json!({"model":MODEL})).await;
        assert_eq!(status, StatusCode::OK, "{unloaded}");
        assert_eq!(
            server.get("juiced/state").await["resources"]["reserved_bytes"],
            0
        );
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(std::fs::read(&server.config_path).unwrap(), original);
        server.restart().await;
    }
    assert_eq!(digest(&engine), engine_sha);
    assert_eq!(digest(&model), model_sha);
    let manager_pid = server.pid();
    server.stop();
    assert!(!std::path::Path::new(&format!("/proc/{manager_pid}")).exists());
    println!(
        "real-embeddings-receipt {}",
        json!({"scope":"Selected llama.cpp CPU Stories15M mean-pooled vector transport; no embedding quality, native recipe, model fit or accelerator qualification","engine_sha256":engine_sha,"model_sha256":model_sha,"worker_pids":worker_pids,"batch_requests":batches,"single_requests":2,"vectors":2*batches+2+gateway_vectors,"gateway_requests":gateway_requests,"dimensions":dimensions,"prefixes":PREFIXES,"invalid_input_rejected_without_reload":true,"input_configuration_unchanged":true,"workers_and_manager_exited":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real GGUF assets; selected chat/responses/client gateway transport, not output quality"]
async fn tiny_gguf_chat_responses_and_client_gateways_keep_public_identity() {
    use reqwest::Method;
    use serde_json::Value;
    use support::{PREFIXES, filesystem::digest};
    const MODEL: &str = "tiny-real-chat";
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let engine_sha = digest(&engine);
    let model_sha = digest(&model);
    let mut server = Server::configured(true, |config| {
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--threads","2","--slots","--n-gpu-layers","0","--chat-template","chatml"],"resident_bytes":512u64 * 1024 * 1024,"max_inflight":1,"operations":["completions","chat/completions","responses","messages/count_tokens"],"policy":"on-demand"}]);
    }).await;
    let original = std::fs::read(&server.config_path).unwrap();
    let messages = json!([{"role":"user","content":"Once upon a time"}]);
    let mut responses = 0;
    for prefix in PREFIXES {
        for (operation, body) in [
            (
                "chat/completions",
                json!({"model":MODEL,"messages":messages,"max_tokens":2,"ignore_eos":true,"stream":false}),
            ),
            (
                "responses",
                json!({"model":MODEL,"input":"Once upon a time","max_output_tokens":2,"ignore_eos":true,"stream":false}),
            ),
        ] {
            let (status, body) = server
                .json(
                    Method::POST,
                    &format!("{prefix}/{operation}"),
                    Some(body),
                    server.read_key(),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{operation}: {body}");
            assert_eq!(body["model"], MODEL);
            if operation == "responses" {
                assert_eq!(body["object"], "response");
                assert_eq!(body["status"], "completed");
                assert!(
                    body["output"]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty())
                );
                assert_eq!(body["usage"]["output_tokens"], 2);
            } else {
                assert_eq!(body["choices"].as_array().unwrap().len(), 1);
                assert!(body["choices"][0]["message"]["content"].is_string());
                assert_eq!(body["usage"]["completion_tokens"], 2);
            }
            responses += 1;
        }
    }
    let pid = server.model(MODEL).await["pid"].as_u64().unwrap();
    let mut streaming_frames = 0;
    for operation in ["chat/completions", "responses"] {
        let body = if operation == "responses" {
            json!({"model":MODEL,"input":"Once upon a time","max_output_tokens":2,"ignore_eos":true,"stream":true})
        } else {
            json!({"model":MODEL,"messages":messages,"max_tokens":2,"ignore_eos":true,"stream":true})
        };
        let response = server
            .client
            .post(format!("{}/v1/{operation}", server.base))
            .bearer_auth(server.read_key().unwrap())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let wire = response.text().await.unwrap();
        assert!(wire.len() <= 65536);
        let mut completed = false;
        let mut observed_alias = false;
        for data in wire.lines().filter_map(|line| line.strip_prefix("data:")) {
            let data = data.trim();
            if data == "[DONE]" {
                completed = true;
                continue;
            }
            let frame: Value = serde_json::from_str(data).unwrap();
            assert!(frame.get("error").is_none(), "{frame}");
            if let Some(model) = frame.get("model").or_else(|| {
                frame
                    .get("response")
                    .and_then(|response| response.get("model"))
            }) {
                assert_eq!(model, MODEL);
                observed_alias = true;
            }
            if frame["type"] == "response.completed" {
                completed = true;
            }
            streaming_frames += 1;
        }
        assert!(completed && observed_alias, "{operation}: {wire}");
    }
    let mut gateway_routes = Vec::new();
    for (path, body) in [
        (
            "/v1/messages",
            json!({"model":MODEL,"messages":messages,"max_tokens":2,"stream":false}),
        ),
        (
            "/v1/messages/count_tokens",
            json!({"model":MODEL,"messages":messages}),
        ),
        (
            "/api/chat",
            json!({"model":MODEL,"messages":messages,"options":{"num_predict":2},"stream":false}),
        ),
        (
            "/api/generate",
            json!({"model":MODEL,"prompt":"Once upon a time","options":{"num_predict":2},"stream":false}),
        ),
    ] {
        let (status, output) = server
            .json(Method::POST, path, Some(body), server.read_key())
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {output}");
        if path.ends_with("count_tokens") {
            assert!(
                output["input_tokens"]
                    .as_u64()
                    .is_some_and(|tokens| tokens > 0)
            );
        } else {
            assert_eq!(output["model"], MODEL);
            if path == "/v1/messages" {
                assert_eq!(output["type"], "message");
                assert!(
                    output["content"]
                        .as_array()
                        .is_some_and(|content| !content.is_empty())
                );
                assert!(
                    output["usage"]["output_tokens"]
                        .as_u64()
                        .is_some_and(|tokens| tokens > 0)
                );
            } else {
                assert_eq!(output["done"], true);
                assert!(
                    output["eval_count"]
                        .as_u64()
                        .is_some_and(|tokens| tokens > 0)
                );
            }
        }
        gateway_routes.push(path);
    }
    let mut gateway_stream_frames = 0;
    for (path, body) in [
        (
            "/v1/messages",
            json!({"model":MODEL,"messages":messages,"max_tokens":2,"stream":true}),
        ),
        (
            "/api/chat",
            json!({"model":MODEL,"messages":messages,"options":{"num_predict":2},"stream":true}),
        ),
        (
            "/api/generate",
            json!({"model":MODEL,"prompt":"Once upon a time","options":{"num_predict":2},"stream":true}),
        ),
    ] {
        let response = server
            .client
            .post(format!("{}{path}", server.base))
            .bearer_auth(server.read_key().unwrap())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let wire = response.text().await.unwrap();
        assert!(wire.len() <= 65536);
        let mut completed = false;
        let mut observed_alias = false;
        for line in wire.lines().filter(|line| !line.is_empty()) {
            let data = if path == "/v1/messages" {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                data.trim()
            } else {
                line
            };
            let frame: Value = serde_json::from_str(data).unwrap();
            assert!(frame.get("error").is_none(), "{frame}");
            if path == "/v1/messages" {
                if frame["type"] == "message_start" {
                    assert_eq!(frame["message"]["model"], MODEL);
                    observed_alias = true;
                }
                if frame["type"] == "message_stop" {
                    completed = true;
                }
            } else {
                assert_eq!(frame["model"], MODEL);
                observed_alias = true;
                completed |= frame["done"] == true;
            }
            gateway_stream_frames += 1;
        }
        assert!(completed && observed_alias, "{path}: {wire}");
    }
    for (name, arguments) in [
        (
            "lemonade_chat",
            json!({"model":MODEL,"messages":messages,"max_tokens":2}),
        ),
        (
            "lemonade_complete",
            json!({"model":MODEL,"prompt":"Once upon a time","max_tokens":2}),
        ),
    ] {
        let(status, output)=server.json(Method::POST,"/mcp",Some(json!({"jsonrpc":"2.0","id":name,"method":"tools/call","params":{"name":name,"arguments":arguments}})),server.read_key()).await;
        assert_eq!(status, StatusCode::OK, "{output}");
        assert_eq!(output["id"], name);
        assert_eq!(output["result"]["isError"], false, "{output}");
        assert_eq!(output["result"]["structuredContent"]["model"], MODEL);
        assert!(
            output["result"]["structuredContent"]["usage"]["completion_tokens"]
                .as_u64()
                .is_some_and(|tokens| tokens > 0)
        );
    }
    server
        .wait_for(async || server.get("juiced/state").await["resources"]["active_requests"] == 0)
        .await;
    assert_eq!(server.model(MODEL).await["pid"], pid);
    let (status, unloaded) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK, "{unloaded}");
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(std::fs::read(&server.config_path).unwrap(), original);
    assert_eq!(digest(&engine), engine_sha);
    assert_eq!(digest(&model), model_sha);
    let manager_pid = server.pid();
    server.stop();
    assert!(!std::path::Path::new(&format!("/proc/{manager_pid}")).exists());
    println!(
        "real-client-gateways-receipt {}",
        json!({"scope":"Selected CPU chatml GGUF OpenAI Chat/Responses JSON and SSE, Anthropic text/count, Ollama text and MCP inference transport; no tool generation, output quality, accelerator or broader client qualification","engine_sha256":engine_sha,"model_sha256":model_sha,"openai_json_requests":responses,"streaming_frames":streaming_frames,"gateway_stream_frames":gateway_stream_frames,"gateway_routes":gateway_routes,"mcp_inference_tools":2,"worker_pid":pid,"same_worker_generation":true,"inputs_configuration_unchanged":true,"workers_and_manager_exited":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real GGUF assets; primary-only model revision and cache pipeline, not quality or runtime package promotion"]
async fn tiny_gguf_artifact_set_load_cache_restart_restore_pipeline() {
    use support::{artifacts::Source, filesystem::digest};
    const MODEL: &str = "tiny-real-artifact-set";
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let metadata = std::fs::metadata(&model).unwrap();
    assert!(metadata.is_file() && metadata.len() <= 64 * 1024 * 1024);
    let engine_sha = digest(&engine);
    let model_sha = digest(&model);
    let source = Source::gated_with_bytes(std::fs::read(&model).unwrap(), 0).await;
    source.finish();
    let mut server = Server::configured(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
        config["cache_enabled"] = json!(true);
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
    })
    .await;
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let (status, job) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":generation,"download":source.download()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let path = format!("juiced/artifacts/jobs/{}", job["id"].as_str().unwrap());
    server
        .wait_for(async || {
            let job = server.get(&path).await;
            job["state"] == "complete" && job["writer_active"] == false
        })
        .await;
    assert_eq!(server.get(&path).await["artifact"], model_sha);
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let(status,set)=server.post("juiced/artifacts/sets",json!({"expected_generation":generation,"specification":{"schema_version":1,"origin":{"importer_id":"juiced-real-gguf-test","importer_version":"1","source_revision":format!("sha256:{model_sha}")},"primary":"model","members":[{"name":"model","role":"model","artifact":model_sha,"bytes":metadata.len()}]}})).await;
    assert_eq!(status, StatusCode::OK, "{set}");
    let(status,registered)=server.post("models/register",json!({"id":MODEL,"model_file":"ignored-managed-set-primary.gguf","artifact":model_sha,"artifact_set":set["id"],"provider":"llama-cpp","command":engine,"args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--threads","2","--slots","--n-gpu-layers","0","--slot-save-path","{cache}"],"resident_bytes":512u64*1024*1024,"max_inflight":1,"operations":["completions"],"policy":"disabled","revision":0})).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert_eq!(registered["artifact_set"], set["id"]);
    let (status, enabled) = server
        .post(
            "juiced/policy",
            json!({"model":MODEL,"policy":"on-demand","expected_revision":registered["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    async fn complete(server: &Server) -> serde_json::Value {
        let response=server.client.post(format!("{}/v1/completions",server.base)).bearer_auth(server.admin_key().unwrap()).header("x-juiced-session","real-artifact-set-session").json(&json!({"model":MODEL,"prompt":"Once upon a time","max_tokens":1,"temperature":0,"cache_prompt":true})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let output: serde_json::Value = response.json().await.unwrap();
        assert_eq!(output["model"], MODEL);
        assert_eq!(output["usage"]["completion_tokens"], 1);
        server
            .wait_for(async || {
                server.get("juiced/state").await["resources"]["active_requests"] == 0
            })
            .await;
        output["usage"].clone()
    }
    let first_usage = complete(&server).await;
    let first = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_eq!(first["applied"]["launch_plan"]["artifact_set"], set["id"]);
    let cache = server.get("juiced/cache").await;
    let(status,saved)=server.post("juiced/cache/save",json!({"model":MODEL,"slot":0,"session":"real-artifact-set-session","expected_generation":cache["generation"],"maximum_bytes":16*1024*1024})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(saved["engine"]["n_written"].as_u64().unwrap() > 0);
    let config = std::fs::read(&server.config_path).unwrap();
    assert_eq!(
        server.post("unload", json!({"model":MODEL})).await.0,
        StatusCode::OK
    );
    server.restart().await;
    let(status,restored)=server.post("juiced/cache/restore",json!({"model":MODEL,"slot":0,"session":"real-artifact-set-session","snapshot":saved["snapshot"]["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["engine"]["n_read"], saved["engine"]["n_written"]);
    let continued_usage = complete(&server).await;
    let resumed = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_eq!(
        resumed["applied"]["launch_plan"]["semantic_fingerprint"],
        first["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_ne!(resumed["applied"]["pid"], first["applied"]["pid"]);
    assert_eq!(
        server.post("unload", json!({"model":MODEL})).await.0,
        StatusCode::OK
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), config);
    let mut promoted_spec = set["specification"].clone();
    promoted_spec["origin"]["source_revision"] = json!("real-primary-promotion");
    let generation = server.get("juiced/artifacts").await["generation"].clone();
    let (status, promoted_set) = server
        .post(
            "juiced/artifacts/sets",
            json!({"expected_generation":generation,"specification":promoted_spec}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{promoted_set}");
    let revisions = format!("models/{MODEL}/artifacts");
    let before_revision = server.get(&revisions).await;
    let (status,promoted) = server.post(&revisions,json!({"expected_revision":before_revision["revision"],"change":{"action":"promote","artifact_set":promoted_set["id"]}})).await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    assert_eq!(promoted["policy"], "on-demand");
    assert_eq!(promoted["previous"]["artifact_set"], set["id"]);
    complete(&server).await;
    let promoted_context = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_ne!(
        promoted_context["applied"]["launch_plan"]["semantic_fingerprint"],
        first["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    let cache_before = support::filesystem::tree(&server.directory.path().join("kv-cache"));
    let restore = json!({"model":MODEL,"slot":0,"session":"real-artifact-set-session","snapshot":saved["snapshot"]["id"]});
    let (status, mismatch) = server.post("juiced/cache/restore", restore.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "snapshot_identity_mismatch");
    assert_eq!(
        support::filesystem::tree(&server.directory.path().join("kv-cache")),
        cache_before
    );
    assert_eq!(
        server.post("unload", json!({"model":MODEL})).await.0,
        StatusCode::OK
    );
    server.restart().await;
    let after_restart = server.get(&revisions).await;
    assert_eq!(after_restart["current"], promoted["current"]);
    assert_eq!(after_restart["previous"], promoted["previous"]);
    let (status, rollback) = server
        .post(
            &revisions,
            json!({"expected_revision":after_restart["revision"],"change":{"action":"rollback"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rollback}");
    assert_eq!(rollback["current"]["artifact_set"], set["id"]);
    assert_eq!(rollback["policy"], "on-demand");
    let (status, rollback_restored) = server.post("juiced/cache/restore", restore).await;
    assert_eq!(status, StatusCode::OK, "{rollback_restored}");
    assert_eq!(
        rollback_restored["engine"]["n_read"],
        saved["engine"]["n_written"]
    );
    let rollback_usage = complete(&server).await;
    let rollback_context = server.get(&format!("juiced/context?model={MODEL}")).await;
    assert_eq!(
        rollback_context["applied"]["launch_plan"]["semantic_fingerprint"],
        first["applied"]["launch_plan"]["semantic_fingerprint"]
    );
    assert_eq!(
        server.post("unload", json!({"model":MODEL})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(digest(&engine), engine_sha);
    assert_eq!(digest(&model), model_sha);
    let manager = server.pid();
    server.stop();
    assert!(!std::path::Path::new(&format!("/proc/{manager}")).exists());
    println!(
        "real-artifact-set-receipt {}",
        json!({"scope":"Selected CPU primary-only managed GGUF artifact-set acquisition, registration, model revision promotion/rollback and native cache continuation; no output quality, companion execution, runtime-package promotion or peak calibration","engine_sha256":engine_sha,"model_sha256":model_sha,"set":set["id"],"promoted_set":promoted_set["id"],"member_bytes":metadata.len(),"first_usage":first_usage,"continued_usage":continued_usage,"rollback_usage":rollback_usage,"saved_bytes":saved["engine"]["n_written"],"restored_bytes":restored["engine"]["n_read"],"rollback_read_bytes":rollback_restored["engine"]["n_read"],"launch_identity_stable_on_restart_and_rollback":true,"worker_generation_changed":true,"source_inputs_unchanged":true,"policy_preserved":true,"workers_and_manager_exited":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; automatic session snapshots, not output quality"]
async fn tiny_gguf_automatic_sessions_restore_older_conversations_after_switch_and_restart() {
    use serde_json::Value;
    const MODEL: &str = "tiny-real-sessions";
    async fn chat(server: &Server, messages: &mut Vec<Value>, question: &str) -> (Value, Value) {
        messages.push(json!({"role":"user","content":question}));
        let (status, reply) = server
            .json(
                reqwest::Method::POST,
                "/v1/chat/completions",
                Some(json!({"model":MODEL,"messages":messages,"max_tokens":2,"temperature":0,"stream":false})),
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        messages
            .push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
        let record = server.get("juiced/requests").await["data"][0].clone();
        (reply, record)
    }
    async fn settled(server: &Server, expected: u64) {
        for _ in 0..600 {
            let view = server.get("juiced/cache/sessions").await;
            let row = view["models"][0].clone();
            if view["totals"]["sessions"] == expected
                && row["slots"][0]["state"] != "unsaved"
                && row["slots"][0]["state"] != "busy"
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("session snapshots did not settle");
    }
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let mut server = Server::configured(true, |config| {
        config["session_cache"] = json!({"save_delay_ms": 50});
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(60000);
        config["request_timeout_ms"] = json!(60000);
        config["models"] = json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","1024","--parallel","1","--threads","4","--slots","--n-gpu-layers","0","--chat-template","chatml"],"resident_bytes":512u64 * 1024 * 1024,"max_inflight":1,"operations":["chat/completions"],"policy":"on-demand"}]);
    })
    .await;
    let mut a: Vec<Value> = Vec::new();
    let mut b: Vec<Value> = Vec::new();
    let (first, record) = chat(&server, &mut a, "River?").await;
    assert_eq!(record["session_cache"]["path"], "cold");
    let prompt = first["usage"]["prompt_tokens"].as_u64().unwrap();
    assert!(prompt > 10, "{first}");
    settled(&server, 1).await;
    let (_, record) = chat(&server, &mut b, "Mountain?").await;
    assert_eq!(record["session_cache"]["path"], "cold");
    settled(&server, 2).await;
    let (restored, record) = chat(&server, &mut a, "Second?").await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    let cached = record["cached_tokens"].as_u64().unwrap();
    let processed = record["prompt_processed_tokens"].as_u64().unwrap();
    assert!(cached >= prompt, "{restored}");
    assert_eq!(
        cached + processed,
        restored["usage"]["prompt_tokens"].as_u64().unwrap(),
        "{restored}"
    );
    settled(&server, 2).await;
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    server.restart().await;
    let (after, record) = chat(&server, &mut a, "Third?").await;
    assert_eq!(record["session_cache"]["path"], "restored", "{record}");
    assert!(
        record["cached_tokens"].as_u64().unwrap() > cached,
        "{after}"
    );
    assert_eq!(
        record["cached_tokens"].as_u64().unwrap()
            + record["prompt_processed_tokens"].as_u64().unwrap(),
        after["usage"]["prompt_tokens"].as_u64().unwrap(),
        "{after}"
    );
    settled(&server, 2).await;
    println!(
        "real-session-receipt {}",
        json!({"first_prompt_tokens":prompt,"restored_cached":cached,"restored_processed":processed,"after_restart":record})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; automatic session snapshots, not output quality"]
async fn tiny_gguf_sessions_restore_after_options_reload_and_idle_unload() {
    use serde_json::Value;
    const MODEL: &str = "tiny-real-reloads";
    async fn chat(server: &Server, messages: &mut Vec<Value>, question: &str) -> Value {
        messages.push(json!({"role":"user","content":question}));
        let (status, reply) = server
            .json(
                reqwest::Method::POST,
                "/v1/chat/completions",
                Some(json!({"model":MODEL,"messages":messages,"max_tokens":2,"temperature":0,"stream":false})),
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        messages
            .push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
        server.get("juiced/requests").await["data"][0].clone()
    }
    fn restored(record: &Value, earlier: u64) -> u64 {
        assert_eq!(record["session_cache"]["path"], "restored", "{record}");
        let cached = record["cached_tokens"].as_u64().unwrap();
        assert!(cached > earlier, "{record}");
        cached
    }
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let server = Server::configured(true, |config| {
        config["session_cache"] = json!({"save_delay_ms": 60000});
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["idle_unload_after_ms"] = json!(1500);
        config["startup_timeout_ms"] = json!(60000);
        config["request_timeout_ms"] = json!(60000);
        config["models"] = json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","1024","--parallel","1","--slots","--n-gpu-layers","0","--chat-template","chatml"],"options":{"threads":4},"resident_bytes":512u64 * 1024 * 1024,"max_inflight":1,"operations":["chat/completions"],"policy":"on-demand"}]);
    })
    .await;
    let mut conversation: Vec<Value> = Vec::new();
    let first = chat(&server, &mut conversation, "River?").await;
    assert_eq!(first["session_cache"]["path"], "cold", "{first}");
    let before = server.get(&format!("models/{MODEL}/options")).await;
    let (status, reloaded) = server
        .post(
            &format!("models/{MODEL}/options"),
            json!({"expected_revision":before["revision"],"options":before["saved"],"apply":"reload"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(reloaded["restarted"], true, "{reloaded}");
    let after_reload = chat(&server, &mut conversation, "Second?").await;
    let cached = restored(&after_reload, 0);
    server
        .wait_for(async || server.model(MODEL).await["loaded"] == false)
        .await;
    assert_eq!(
        server.model(MODEL).await["residency"]["last_unload"]["reason"],
        "idle"
    );
    let after_idle = chat(&server, &mut conversation, "Third?").await;
    restored(&after_idle, cached);
    println!(
        "real-reload-session-receipt {}",
        json!({"cold":first,"after_options_reload":after_reload,"after_idle_unload":after_idle})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_PREFIX_GGUF with at least 4k context; prefix reuse across conversations, not output quality"]
async fn real_new_conversations_reuse_saved_system_prompts_across_clients_and_restart() {
    use serde_json::Value;
    const MODEL: &str = "real-prefix-sessions";
    let variable = |name: &str| std::env::var(name).ok();
    let engine =
        PathBuf::from(variable("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"));
    let Some(model) = variable("JUICED_TEST_PREFIX_GGUF").map(PathBuf::from) else {
        println!("skipped: set JUICED_TEST_PREFIX_GGUF to a chat GGUF with at least 4k context");
        return;
    };
    let lines: usize =
        variable("JUICED_TEST_PROMPT_LINES").map_or(40, |value| value.parse().unwrap());
    let slots: u32 = variable("JUICED_TEST_SLOTS").map_or(1, |value| value.parse().unwrap());
    let resident: u64 =
        variable("JUICED_TEST_RESIDENT_GIB").map_or(1, |value| value.parse().unwrap());
    let template = variable("JUICED_TEST_CHAT_TEMPLATE");
    let mut args = vec![
        "--host",
        "127.0.0.1",
        "--port",
        "{port}",
        "--model",
        "{model}",
        "--ctx-size",
        "8192",
        "--slots",
        "--n-gpu-layers",
        "0",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match &template {
        Some(name) => args.extend(["--chat-template".to_owned(), name.clone()]),
        None => args.push("--jinja".to_owned()),
    }
    let parallel = slots.to_string();
    args.extend(["--parallel".to_owned(), parallel]);
    let gib = 1024u64 * 1024 * 1024;
    let mut server = Server::configured(true, |config| {
        config["session_cache"] = json!({"save_delay_ms": 50});
        config["resident_budget_bytes"] = json!((resident + 2) * gib);
        config["startup_timeout_ms"] = json!(300000);
        config["request_timeout_ms"] = json!(1800000);
        config["models"] = json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":args,"resident_bytes":resident * gib,"max_inflight":slots,"operations":["chat/completions"],"policy":"on-demand"}]);
    })
    .await;
    let words = [
        "kitchen",
        "porch",
        "garage",
        "hallway",
        "office",
        "bedroom",
        "thermostat",
        "sensor",
        "lamp",
        "fan",
        "door",
        "switch",
        "blind",
    ];
    let system = |client: &str, live: Option<u32>| {
        let mut rows = (0..lines)
            .map(|index| {
                format!(
                    "{client} device {index}: {} {} in area {} is exposed as light.device_{index}.",
                    words[index % words.len()],
                    words[(index * 7) % words.len()],
                    index % 11
                )
            })
            .collect::<Vec<_>>();
        if let Some(value) = live {
            rows[lines / 2] = format!(
                "Live note: {value} GiB available, {} GiB reserved.",
                value * 3
            );
        }
        rows.join("\n")
    };
    async fn settle(server: &Server) {
        for _ in 0..1200 {
            let view = server.get("juiced/cache/sessions").await;
            let idle = view["models"][0]["slots"].as_array().is_some_and(|slots| {
                slots
                    .iter()
                    .all(|slot| slot["state"] != "unsaved" && slot["state"] != "busy")
            });
            if idle {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("session snapshots did not settle");
    }
    let mut receipts = Vec::new();
    let mut ask = async |server: &Server, step: &str, system: String, question: &str| -> Value {
        let mut request = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap()
            .post(format!("{}/v1/chat/completions", server.base))
            .json(&json!({"model":MODEL,"messages":[{"role":"system","content":system},{"role":"user","content":question}],"max_tokens":2,"temperature":0,"stream":false}));
        if let Some(key) = server.read_key() {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _: Value = response.json().await.unwrap();
        let record = server.get("juiced/requests").await["data"][0].clone();
        let receipt = json!({"step":step,"path":record["session_cache"]["path"],"slot":record["session_cache"]["slot"],"input":record["input_tokens"],"cached":record["cached_tokens"],"processed":record["prompt_processed_tokens"],"elapsed_ms":record["elapsed_ms"],"restore_ms":record["session_cache"]["restore_ms"]});
        println!("prefix-receipt {receipt}");
        settle(server).await;
        receipts.push(receipt);
        record
    };
    ask(
        &server,
        "a1-cold",
        system("Alpha", None),
        "Turn on the porch light.",
    )
    .await;
    ask(
        &server,
        "b1-other-client",
        system("Bravo", None),
        "Close the garage.",
    )
    .await;
    let returning = ask(
        &server,
        "a2-new-conversation",
        system("Alpha", None),
        "Is the office warm?",
    )
    .await;
    ask(
        &server,
        "b2-new-conversation",
        system("Bravo", None),
        "Dim the lamp.",
    )
    .await;
    ask(
        &server,
        "a3-new-conversation",
        system("Alpha", None),
        "Open the blind.",
    )
    .await;
    let (status, _) = server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    server.restart().await;
    ask(
        &server,
        "load-after-restart",
        "Be brief.".to_owned(),
        "Ready?",
    )
    .await;
    let restarted = ask(
        &server,
        "a4-after-restart",
        system("Alpha", None),
        "Lock the door.",
    )
    .await;
    ask(
        &server,
        "a5-middle-changed",
        system("Alpha", Some(4)),
        "Fan speed?",
    )
    .await;
    ask(
        &server,
        "a6-middle-changed-again",
        system("Alpha", Some(5)),
        "Fan off?",
    )
    .await;
    ask(
        &server,
        "a7-end-changed",
        system("Alpha", None) + "\nLive note: 9 GiB available.",
        "Hallway?",
    )
    .await;
    println!("prefix-receipts {}", Value::Array(receipts));
    let expected = if slots > 1 { "slot-hit" } else { "restored" };
    assert_eq!(returning["session_cache"]["path"], expected, "{returning}");
    assert_eq!(
        restarted["session_cache"]["path"], "restored",
        "{restarted}"
    );
    for record in [&returning, &restarted] {
        assert!(
            record["cached_tokens"].as_u64().unwrap() * 2
                > record["input_tokens"].as_u64().unwrap(),
            "{record}"
        );
    }
}
