mod support;

use reqwest::{Method, StatusCode, header};
use serde_json::{Value, json};
use std::path::PathBuf;
use support::{
    Server,
    gguf::{self, Entry},
};

const TEMPLATE: &str = "{%- if tools %}<|im_start|>system\n{% for tool in tools %}{{ tool | tojson }}{% endfor %}<|im_end|>\n{% endif %}{%- for message in messages %}<|im_start|>{{ message.role }}\n{{ message.content }}{%- if message.tool_calls %}{% for tool_call in message.tool_calls %}<tool_call>{{ tool_call.function.name }}</tool_call>{% endfor %}{% endif %}<|im_end|>\n{% endfor %}{%- if add_generation_prompt %}<|im_start|>assistant\n{%- if enable_thinking is defined and enable_thinking is false %}\n<think>\n\n</think>\n\n{%- endif %}{%- endif %}";

fn entry<'a>(list: &'a Value, id: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == id)
        .unwrap_or_else(|| panic!("{id} is listed"))
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn read(server: &Server, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
    let mut request = server
        .client
        .get(format!("{}{path}", server.base))
        .bearer_auth(server.read_key().unwrap());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn discovery_routes_publish_native_fields_revalidate_and_answer_read_keys() {
    let directory = tempfile::tempdir().unwrap();
    let weights = directory.path().join("templated.gguf");
    std::fs::write(
        &weights,
        gguf::build(&[
            ("general.architecture", Entry::Text("llama")),
            ("llama.context_length", Entry::U32(32768)),
            ("tokenizer.chat_template", Entry::Text(TEMPLATE)),
        ]),
    )
    .unwrap();
    let worker = support::binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let server = Server::configured(true, |config| {
        let models = config["models"].as_array_mut().unwrap();
        models.push(json!({
            "id":"templated", "model_file":weights, "command":worker, "provider":"llama-cpp",
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","16384","--parallel","2","--predict","2048"],
            "resident_bytes":64, "max_inflight":2, "operations":["chat/completions"], "policy":"on-demand",
        }));
    })
    .await;

    let health = server.get("health").await;
    assert_eq!(health["status"], "ok");
    assert!(health["version"].is_string());
    assert_eq!(health["model_loaded"], Value::Null);
    assert_eq!(health["all_models_loaded"], json!([]));
    assert_eq!(health["max_models"]["llm"], 3);
    assert_eq!(health["pinned_models"]["llm"], 0);

    let response = read(&server, "/v1/juiced/catalog", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let tag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(tag.starts_with("W/\""));
    let catalog: Value = response.json().await.unwrap();
    assert_eq!(catalog["object"], "list");
    assert_eq!(catalog["discovery_schema_version"], 1);
    let revision = catalog["revision"].as_str().unwrap().to_owned();
    let templated = entry(&catalog["data"], "templated");
    assert_eq!(templated["recipe"], "llamacpp");
    assert_eq!(templated["context"]["declared_tokens"], 32768);
    assert_eq!(templated["context"]["configured_tokens"], 16384);
    assert_eq!(templated["context"]["configured_slots"], 2);
    assert_eq!(templated["context"]["per_request_tokens"], 8192);
    assert_eq!(templated["context"]["per_request_source"], "configured");
    assert_eq!(
        templated["context"]["advertised_source"],
        "configured_options"
    );
    assert_eq!(
        templated["context"]["observed_per_request_tokens"],
        Value::Null
    );
    assert_eq!(templated["context_length"], 8192);
    assert_eq!(templated["max_context_window"], 32768);
    assert_eq!(templated["max_output_tokens"], 2048);
    assert_eq!(templated["thinking"]["supported"], true);
    assert_eq!(templated["thinking"]["source"], "gguf_chat_template");
    assert_eq!(
        templated["thinking"]["request_control"],
        "chat_template_kwargs.enable_thinking"
    );
    assert_eq!(templated["thinking"]["default"], "on");
    assert_eq!(templated["tool_calls"]["supported"], true);
    assert_eq!(templated["vision"], false);
    assert_eq!(
        templated["labels"],
        json!(["chat", "reasoning", "tool-calling"])
    );
    let contract = &templated["capability_contract"];
    assert_eq!(contract["features"]["reasoning"], "declared");
    assert_eq!(contract["features"]["tools"], "declared");
    assert_eq!(contract["features"]["vision"], Value::Null);
    assert_eq!(contract["limits"]["per_request_context_tokens"], 8192);
    let fixture = entry(&catalog["data"], "fixture.one");
    assert_eq!(fixture["recipe"], Value::Null);
    assert_eq!(fixture["thinking"]["supported"], Value::Null);
    assert_eq!(fixture["thinking"]["reason"], "provider_unobserved");
    assert_eq!(fixture["context"]["per_request_tokens"], Value::Null);
    assert_eq!(fixture["context"]["advertised_source"], "unknown");

    let unchanged = read(&server, "/v1/juiced/catalog", &[("if-none-match", &tag)]).await;
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(unchanged.headers()[header::ETAG], tag.as_str());
    assert!(unchanged.bytes().await.unwrap().is_empty());

    let response = read(&server, "/v1/models", &[]).await;
    let models_tag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let models: Value = response.json().await.unwrap();
    assert_eq!(models["object"], "list");
    assert_eq!(
        ids(&models["data"]),
        ["fixture.one", "fixture.two", "templated"]
    );
    let listed = entry(&models["data"], "templated");
    assert_eq!(listed["object"], "model");
    assert_eq!(listed["owned_by"], "juiced");
    assert_eq!(listed["downloaded"], true);
    assert_eq!(listed["context_length"], 8192);
    assert_eq!(listed["context"]["per_request_source"], "configured");
    assert_eq!(
        listed["labels"],
        json!(["chat", "reasoning", "tool-calling"])
    );
    assert_eq!(
        read(&server, "/v1/models", &[("if-none-match", &models_tag)])
            .await
            .status(),
        StatusCode::NOT_MODIFIED
    );
    let all: Value = read(&server, "/v1/models?show_all=true", &[])
        .await
        .json()
        .await
        .unwrap();
    assert!(ids(&all["data"]).contains(&"fixture.disabled".to_owned()));
    let speech: Value = read(&server, "/v1/models?service=speak", &[])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(speech["data"], json!([]));
    assert_eq!(
        read(&server, "/v1/models?service=unknown", &[])
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let anthropic: Value = read(
        &server,
        "/v1/models",
        &[("anthropic-version", "2023-06-01")],
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(anthropic["data"][0]["type"], "model");
    assert_eq!(anthropic["has_more"], false);
    assert_eq!(anthropic["first_id"], "fixture.one");

    let detail = server.get("models/templated").await;
    assert_eq!(detail["id"], "templated");
    assert_eq!(detail["context_length"], 8192);
    assert_eq!(detail["juiced"]["id"], "templated");

    let response = read(&server, "/v1/juiced/capabilities", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let capabilities: Value = response.json().await.unwrap();
    assert_eq!(capabilities["object"], "juiced.capabilities");
    assert_eq!(capabilities["schema_version"], 1);
    assert_eq!(capabilities["implementation"], "rust-experiment");
    assert_eq!(capabilities["revision"], revision.as_str());
    let models_with_contracts: Vec<String> = capabilities["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["model"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        models_with_contracts,
        ["fixture.one", "fixture.two", "templated"]
    );
    let contract = &capabilities["models"][2];
    assert_eq!(contract["policy"], "on-demand");
    assert_eq!(contract["thinking"]["supported"], true);
    assert_eq!(contract["tool_calls"]["supported"], true);
    assert!(capabilities["protocols"].as_array().unwrap().len() > 3);

    for path in [
        "/.well-known/juiced.json",
        "/v1/juiced/capabilities",
        "/v1/juiced/context?model=templated",
    ] {
        let response = server
            .client
            .get(format!("{}{path}", server.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    let known: Value = read(&server, "/.well-known/juiced.json", &[])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(known["name"], "Juiced");
    assert_eq!(known["capability_schema"], 1);
    assert_eq!(known["catalog"], "/v1/juiced/catalog");
    assert_eq!(known["capabilities"], "/v1/juiced/capabilities");

    let endpoints = server.get("juiced/endpoints").await;
    let authority = |path: &str| {
        endpoints["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|endpoint| endpoint["path"] == path && endpoint["method"] == "GET")
            .map(|endpoint| endpoint["authority"].clone())
    };
    assert_eq!(authority("/juiced/context"), Some(json!("read")));
    assert_eq!(authority("/juiced/capabilities"), Some(json!("read")));
    assert_eq!(authority("/.well-known/juiced.json"), Some(json!("read")));

    let context: Value = read(&server, "/v1/juiced/context?model=templated", &[])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(context["observation_error"], "worker_unloaded");
    assert_eq!(context["per_request_tokens"], 8192);
    assert_eq!(context["per_request_source"], "configured");
    assert_eq!(context["discovery"]["configured_slots"], 2);

    let (status, loaded) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    let health = server.get("health").await;
    assert_eq!(health["model_loaded"], "fixture.one");
    let loaded = &health["all_models_loaded"][0];
    assert_eq!(loaded["model_name"], "fixture.one");
    assert_eq!(loaded["type"], "llm");
    assert_eq!(loaded["loaded"], true);
    assert_eq!(loaded["status"], "ready");
    assert!(loaded["pid"].as_u64().is_some());
    let context: Value = read(&server, "/v1/juiced/context?model=fixture.one", &[])
        .await
        .json()
        .await
        .unwrap();
    assert!(context["applied"].get("pid").is_none(), "{context}");
    assert!(context["applied"]["launch"].is_object() || context["applied"]["launch"].is_null());
    let (_, operator) = server
        .json(
            Method::GET,
            "/v1/juiced/context?model=fixture.one",
            None,
            server.admin_key(),
        )
        .await;
    assert!(operator["applied"]["pid"].as_u64().is_some());

    let changed = read(&server, "/v1/juiced/catalog", &[("if-none-match", &tag)]).await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()[header::ETAG], tag.as_str());
    let changed: Value = changed.json().await.unwrap();
    assert_ne!(changed["revision"], revision.as_str());
    assert_eq!(
        read(&server, "/v1/models", &[("if-none-match", &models_tag)])
            .await
            .status(),
        StatusCode::OK
    );
    let model = server.model("templated").await;
    let (status, value) = server
        .post(
            "juiced/policy",
            json!({"model":"templated","policy":"disabled","expected_revision":model["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let after: Value = read(&server, "/v1/models", &[]).await.json().await.unwrap();
    assert!(!ids(&after["data"]).contains(&"templated".to_owned()));
    assert_ne!(after["revision"], changed["revision"]);
}

fn with_training_context(mut bytes: Vec<u8>, tokens: u32) -> Vec<u8> {
    let key = b"llama.context_length";
    let at = bytes
        .windows(key.len())
        .position(|window| window == key)
        .expect("training context key")
        + key.len();
    assert_eq!(&bytes[at..at + 4], &4u32.to_le_bytes());
    bytes[at + 4..at + 8].copy_from_slice(&tokens.to_le_bytes());
    bytes
}

/// Runs the pi-t3-agent discovery reader (`JUICED_TEST_PI_CATALOG` names its catalog.mjs).
fn pi_catalog(server: &Server) -> Option<Value> {
    let reader = std::env::var_os("JUICED_TEST_PI_CATALOG")?;
    let script = format!(
        "import {{ discover }} from {reader:?}; const result = await discover({{ baseUrl: {base:?}, apiKey: {key:?}, mode: 'juiced' }}); console.log(JSON.stringify(result));",
        reader = PathBuf::from(reader).display().to_string(),
        base = format!("{}/v1", server.base),
        key = server.read_key().unwrap(),
    );
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    println!("pi-t3-agent catalog reader: {result}");
    assert_eq!(result["source"], "juiced");
    Some(result)
}

fn real_assets() -> (PathBuf, PathBuf) {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    assert!(engine.is_file() && model.is_file());
    (engine, model)
}

async fn chat_prompt_tokens(server: &Server, body: Value) -> u64 {
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(body),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply["usage"]["prompt_tokens"].as_u64().unwrap()
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; real llama-server context and template evidence"]
async fn real_llama_server_context_capabilities_and_thinking_control() {
    let (engine, model) = real_assets();
    let directory = tempfile::tempdir().unwrap();
    let templated = directory.path().join("stories-template.gguf");
    let source = std::fs::read(&model).unwrap();
    std::fs::write(
        &templated,
        gguf::with_text(&source, "tokenizer.chat_template", TEMPLATE),
    )
    .unwrap();
    let longer = directory.path().join("stories-4096.gguf");
    std::fs::write(&longer, with_training_context(source, 4096)).unwrap();
    let args = |context: &str, slots: &str| {
        json!([
            "--host",
            "127.0.0.1",
            "--port",
            "{port}",
            "--model",
            "{model}",
            "--ctx-size",
            context,
            "--parallel",
            slots,
            "--threads",
            "2",
            "--slots",
            "--n-gpu-layers",
            "0"
        ])
    };
    let mut server = Server::configured(true, |config| {
        config["resident_budget_bytes"] = json!(2048u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([
            {"id":"stories-template","model_file":templated,"command":engine,"provider":"llama-cpp","args":args("1024","2"),
             "resident_bytes":512u64*1024*1024,"max_inflight":2,"operations":["chat/completions","completions"],"policy":"on-demand"},
            {"id":"stories-slots","model_file":longer,"command":engine,"provider":"llama-cpp","args":args("1000","3"),
             "resident_bytes":512u64*1024*1024,"max_inflight":3,"operations":["chat/completions","completions"],"policy":"on-demand"},
            {"id":"stories-plain","model_file":model,"command":engine,"provider":"llama-cpp","args":args("100","1"),
             "resident_bytes":512u64*1024*1024,"max_inflight":1,"operations":["chat/completions","completions"],"policy":"on-demand"},
        ]);
    })
    .await;
    let catalog = server.get("juiced/catalog").await;
    let unloaded = entry(&catalog["data"], "stories-template");
    assert_eq!(unloaded["context"]["declared_tokens"], 128);
    assert_eq!(unloaded["context"]["per_request_tokens"], 128);
    assert_eq!(unloaded["context"]["per_request_source"], "configured");
    assert_eq!(unloaded["thinking"]["source"], "gguf_chat_template");
    assert_eq!(unloaded["thinking"]["supported"], true);
    assert_eq!(unloaded["tool_calls"]["supported"], true);
    let plain = entry(&catalog["data"], "stories-plain");
    assert_eq!(plain["context"]["per_request_tokens"], 128);
    assert_eq!(plain["thinking"]["supported"], Value::Null);
    assert_eq!(plain["thinking"]["reason"], "no_chat_template");
    let slots = entry(&catalog["data"], "stories-slots");
    assert_eq!(slots["context"]["declared_tokens"], 4096);
    assert_eq!(slots["context"]["per_request_tokens"], 512);

    if let Some(result) = pi_catalog(&server) {
        assert_eq!(
            entry(&result["models"], "stories-slots")["contextWindow"],
            512
        );
        assert_eq!(
            entry(&result["models"], "stories-template")["reasoning"],
            true
        );
    }

    for id in ["stories-template", "stories-plain", "stories-slots"] {
        let (status, loaded) = server.post("load", json!({"model":id})).await;
        assert_eq!(status, StatusCode::OK, "{loaded}");
    }
    let catalog = server.get("juiced/catalog").await;
    let observed = entry(&catalog["data"], "stories-template");
    assert_eq!(observed["context"]["observed_per_request_tokens"], 128);
    assert_eq!(observed["context"]["observed_slots"], 2);
    assert_eq!(observed["context"]["per_request_source"], "observed");
    assert_eq!(
        observed["context"]["advertised_source"],
        "runtime_observation"
    );
    assert_eq!(
        observed["context"]["per_request_tokens"],
        observed["context_length"]
    );
    assert_eq!(observed["thinking"]["source"], "engine_chat_template");
    assert_eq!(observed["thinking"]["supported"], true);
    assert_eq!(observed["tool_calls"]["source"], "engine_template_caps");
    assert_eq!(observed["tool_calls"]["supported"], true);
    let slots = entry(&catalog["data"], "stories-slots");
    assert_eq!(slots["context"]["observed_per_request_tokens"], 512);
    assert_eq!(slots["context"]["observed_slots"], 3);
    let plain = entry(&catalog["data"], "stories-plain");
    assert_eq!(plain["context"]["per_request_source"], "observed");
    assert_eq!(
        plain["context"]["observed_per_request_tokens"],
        plain["context"]["per_request_tokens"]
    );
    assert!(
        plain["thinking"]["source"]
            .as_str()
            .unwrap()
            .starts_with("engine_")
    );
    let context: Value = read(&server, "/v1/juiced/context?model=stories-template", &[])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(context["observed"]["props"]["per_request_tokens"], 128);
    assert_eq!(context["per_request_tokens"], 128);
    let health = server.get("health").await;
    assert_eq!(health["all_models_loaded"].as_array().unwrap().len(), 3);
    assert_eq!(
        entry(&catalog["data"], "stories-template")["context"]["per_request_tokens"],
        health["all_models_loaded"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["model_name"] == "stories-template")
            .unwrap()["context_length"]
    );

    let message = json!([{"role":"user","content":"Hi"}]);
    let request = |extra: Value| {
        let mut body =
            json!({"model":"stories-template","messages":message,"max_tokens":2,"temperature":0});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let on = chat_prompt_tokens(
        &server,
        request(json!({"chat_template_kwargs":{"enable_thinking":true}})),
    )
    .await;
    let off = chat_prompt_tokens(
        &server,
        request(json!({"chat_template_kwargs":{"enable_thinking":false}})),
    )
    .await;
    assert!(
        off > on,
        "disabling thinking adds the empty think block: {off} vs {on}"
    );
    assert_eq!(
        chat_prompt_tokens(&server, request(json!({"enable_thinking":false}))).await,
        off
    );
    assert_eq!(
        chat_prompt_tokens(&server, request(json!({"thinking":{"type":"disabled"}}))).await,
        off
    );
    assert_eq!(
        chat_prompt_tokens(&server, request(json!({"enable_thinking":true}))).await,
        on
    );
    assert_eq!(chat_prompt_tokens(&server, request(json!({}))).await, on);
    let (status, anthropic) = server
        .json(
            Method::POST,
            "/v1/messages",
            Some(json!({"model":"stories-template","max_tokens":2,"messages":[{"role":"user","content":"Hi"}],"thinking":{"type":"disabled"}})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{anthropic}");
    assert_eq!(anthropic["usage"]["input_tokens"], off);

    if let Some(result) = pi_catalog(&server) {
        let model = entry(&result["models"], "stories-template");
        assert_eq!(model["contextWindow"], 128);
        assert_eq!(model["reasoning"], true);
        assert_eq!(model["tools"], true);
        assert_eq!(model["vision"], false);
        assert_eq!(
            entry(&result["models"], "stories-slots")["contextWindow"],
            512
        );
    }
    server.restart().await;
}
