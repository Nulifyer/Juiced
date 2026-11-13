mod support;

use base64::Engine;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use support::{PREFIXES, Server, gguf, speech};

async fn captured_server() -> (Server, PathBuf) {
    let mut path = PathBuf::new();
    let server = Server::configured(true, |config| {
        let directory = PathBuf::from(config["models"][0]["args"][5].as_str().unwrap())
            .parent()
            .unwrap()
            .to_owned();
        path = directory.join("bodies.jsonl");
        for model in config["models"].as_array_mut().unwrap() {
            model["args"]
                .as_array_mut()
                .unwrap()
                .extend([json!("--capture-bodies"), json!(path)]);
        }
    })
    .await;
    (server, path)
}
fn take_bodies(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let _ = std::fs::remove_file(path);
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn post(server: &Server, path: &str, body: Value) -> reqwest::Response {
    server
        .request(Method::POST, path, Some(body), server.read_key())
        .await
        .unwrap()
}
async fn mcp(server: &Server, name: &str, arguments: Value) -> Value {
    let response = post(
        server,
        "/mcp",
        json!({"jsonrpc":"2.0","id":name,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert!(value.get("error").is_none(), "{value}");
    value["result"].clone()
}

#[tokio::test]
async fn native_v092_anthropic_and_ollama_fields_are_ignored_with_warnings() {
    let (server, bodies) = captured_server().await;
    let response = post(
        &server,
        "/v1/messages",
        json!({"model":"fixture.one","max_tokens":8,"metadata":{"user_id":"u"},"service_tier":"auto",
            "system":[{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],
            "thinking":{"type":"enabled","budget_tokens":1024},
            "messages":[{"role":"user","content":[{"type":"text","text":"Hi","cache_control":{"type":"ephemeral"}}]}]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let warning = response.headers()["x-lemonade-warning"]
        .to_str()
        .unwrap()
        .to_owned();
    for field in ["metadata", "service_tier", "cache_control", "budget_tokens"] {
        assert!(warning.contains(&format!("'{field}'")), "{warning}");
    }
    let message: Value = response.json().await.unwrap();
    assert_eq!(message["type"], "message");
    assert!(
        message["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("Ignored 'metadata' field"))
    );
    let [upstream] = take_bodies(&bodies).try_into().unwrap();
    assert!(upstream.get("metadata").is_none());
    assert!(upstream.get("enable_thinking").is_none());
    assert_eq!(upstream["chat_template_kwargs"]["enable_thinking"], true);
    assert_eq!(upstream["messages"][0]["content"], "Be brief.");

    let response = post(
        &server,
        "/api/chat",
        json!({"model":"fixture.one:latest","stream":false,"keep_alive":"5m","think":false,
            "options":{"num_ctx":4096,"min_p":0.05,"repeat_penalty":1.1,"num_thread":8},
            "messages":[{"role":"user","content":"Hi","images":null}]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["x-lemonade-warning"]
            .to_str()
            .unwrap()
            .contains("num_thread")
    );
    let reply: Value = response.json().await.unwrap();
    assert_eq!(reply["done"], true);
    let response = post(
        &server,
        "/api/generate",
        json!({"model":"fixture.one","prompt":"Hi","stream":false,"system":"Be brief.","context":[1,2,3],
            "options":{"repeat_penalty":1.2,"num_predict":-1}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let [chat, generate] = take_bodies(&bodies).try_into().unwrap();
    assert_eq!(chat["min_p"], 0.05);
    assert_eq!(chat["repeat_penalty"], 1.1);
    assert!(chat.get("num_thread").is_none() && chat.get("num_ctx").is_none());
    assert_eq!(chat["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(generate["prompt"], "Hi");
    assert_eq!(generate["repeat_penalty"], 1.2);
    assert!(generate.get("system").is_none() && generate.get("max_tokens").is_none());
}

#[tokio::test]
async fn every_thinking_off_form_reaches_the_engine_as_a_template_argument() {
    let (server, bodies) = captured_server().await;
    let messages = json!([{"role":"user","content":"Hi"}]);
    for extra in [
        json!({"enable_thinking":false}),
        json!({"thinking":false}),
        json!({"thinking":{"type":"disabled"}}),
        json!({"chat_template_kwargs":{"enable_thinking":false}}),
    ] {
        let mut body = json!({"model":"fixture.one","messages":messages,"max_tokens":4});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = post(&server, "/v1/chat/completions", body).await;
        assert_eq!(response.status(), StatusCode::OK, "{extra}");
    }
    let response = post(
        &server,
        "/v1/messages",
        json!({"model":"fixture.one","max_tokens":4,"messages":messages,"thinking":{"type":"disabled"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = post(
        &server,
        "/api/chat",
        json!({"model":"fixture.one","messages":messages,"stream":false,"think":false}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(
        mcp(&server, "lemonade_chat", json!({"messages":messages})).await["isError"],
        true
    );
    let captured = take_bodies(&bodies);
    assert_eq!(captured.len(), 7);
    for body in captured {
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"], false,
            "{body}"
        );
        assert!(body.get("enable_thinking").is_none() && body.get("thinking").is_none());
    }
}

#[tokio::test]
async fn native_v092_refusals_keep_their_status_and_bodies() {
    let server = Server::new(true).await;
    for (method, path, status, error) in [
        (
            Method::POST,
            "/api/pull",
            501,
            "Use Juiced model management to download weights",
        ),
        (
            Method::DELETE,
            "/api/delete",
            501,
            "Use Juiced model management to remove weights",
        ),
        (
            Method::POST,
            "/api/create",
            501,
            "not supported by Lemonade",
        ),
        (Method::POST, "/api/copy", 501, "not supported by Lemonade"),
        (Method::POST, "/api/push", 501, "not supported by Lemonade"),
    ] {
        let (actual, body) = server
            .json(method, path, Some(json!({"model":"x"})), server.read_key())
            .await;
        assert_eq!(actual.as_u16(), status, "{path}");
        assert_eq!(body, json!({"error":error}), "{path}");
    }
    let (status, body) = server
        .json(
            Method::POST,
            "/api/blobs/sha256:00",
            Some(json!({})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body, json!({"error":"not supported by Lemonade"}));
    let jobs = "Juiced does not execute caller-defined workflows or resume legacy graphs";
    let threads = "Juiced does not store conversations. Downstream clients own chat history.";
    for prefix in PREFIXES {
        for (method, suffix, status, body) in [
            (Method::POST, "/jobs", 501, json!({"error":jobs})),
            (Method::GET, "/jobs", 200, json!({"jobs":[]})),
            (Method::GET, "/jobs/j1", 404, json!({"error":"unknown job"})),
            (
                Method::DELETE,
                "/jobs/j1",
                404,
                json!({"error":"unknown job"}),
            ),
            (
                Method::POST,
                "/jobs/j1/pause",
                404,
                json!({"error":"job not found or not pausable"}),
            ),
            (
                Method::POST,
                "/jobs/j1/interrupt",
                404,
                json!({"error":"job not found or not interruptible"}),
            ),
            (Method::POST, "/jobs/j1/resume", 501, json!({"error":jobs})),
            (
                Method::GET,
                "/juiced/threads",
                410,
                json!({"error":threads}),
            ),
            (Method::GET, "/juiced/thread", 410, json!({"error":threads})),
            (
                Method::POST,
                "/juiced/thread",
                410,
                json!({"error":threads}),
            ),
        ] {
            let path = format!("{prefix}{suffix}");
            let (actual, value) = server
                .json(method, &path, Some(json!({})), server.admin_key())
                .await;
            assert_eq!(actual.as_u16(), status, "{path}");
            assert_eq!(value, body, "{path}");
        }
    }
    let response = server
        .request(Method::POST, "/v1/jobs", Some(json!({})), server.read_key())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bundled_api_docs_answer_under_every_prefix_and_through_mcp() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        let (status, index) = server
            .json(
                Method::GET,
                &format!("{prefix}/docs"),
                None,
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(index["format"], "text/markdown");
        let docs = index["docs"].as_array().unwrap();
        let openai = docs.iter().find(|doc| doc["id"] == "api/openai").unwrap();
        assert_eq!(openai["url"], format!("{prefix}/docs/api/openai"));
        assert_eq!(openai["title"], "Use the OpenAI-compatible API");
        for page in ["api/openai", "api/openai.md"] {
            let response = server
                .request(
                    Method::GET,
                    &format!("{prefix}/docs/{page}"),
                    None,
                    server.read_key(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/markdown")
            );
            let text = response.text().await.unwrap();
            assert_eq!(text.len() as u64, openai["bytes"].as_u64().unwrap());
        }
    }
    let (status, missing) = server
        .json(Method::GET, "/v1/docs/api/missing", None, server.read_key())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing, json!({"error":"Documentation page not found"}));
    let response = server
        .request(Method::GET, "/v1/docs", None, None)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let listing = mcp(&server, "lemonade_docs", json!({})).await;
    let pages: Value =
        serde_json::from_str(listing["content"][1]["text"].as_str().unwrap()).unwrap();
    assert!(
        pages["pages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|page| page["id"] == "api/mcp")
    );
    let page = mcp(&server, "lemonade_docs", json!({"page":"api/mcp"})).await;
    assert!(
        page["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("# ")
    );
    let unknown = mcp(&server, "lemonade_docs", json!({"page":"zzz"})).await;
    assert_eq!(unknown["isError"], true);
}

#[tokio::test]
async fn mcp_lists_the_native_tools_and_transcribes_audio() {
    let server = speech::server(|_| {}).await;
    let response = post(
        &server,
        "/mcp",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let tools: Value = response.json().await.unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for native in [
        "lemonade_list_models",
        "lemonade_chat",
        "lemonade_transcribe_audio",
        "lemonade_generate_image",
        "lemonade_docs",
    ] {
        assert!(names.contains(&native), "{native}");
    }
    let audio = base64::engine::general_purpose::STANDARD.encode(speech::audio());
    let result = mcp(
        &server,
        "lemonade_transcribe_audio",
        json!({"audio_base64":audio,"filename":"probe.wav","language":"en"}),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    assert!(result["content"][0]["text"].is_string());
    assert_eq!(
        result["structuredContent"]["received_fields"]["language"],
        "en"
    );
    assert_eq!(
        result["structuredContent"]["received_file"]["bytes"],
        speech::audio().len()
    );
    let text = mcp(
        &server,
        "lemonade_transcribe_audio",
        json!({"model":"fixture.one","audio_base64":audio,"response_format":"text"}),
    )
    .await;
    assert_eq!(text["isError"], false, "{text}");
    let refused = mcp(
        &server,
        "lemonade_transcribe_audio",
        json!({"audio_path":"/etc/hostname"}),
    )
    .await;
    assert_eq!(refused["isError"], true);
    let missing = mcp(&server, "lemonade_generate_image", json!({"prompt":"cat"})).await;
    assert_eq!(missing["isError"], true);
    let status: Value =
        serde_json::from_str(missing["content"][1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["status"], "needs_model");
}

#[tokio::test]
async fn mcp_generates_inline_png_images() {
    let server = Server::configured(true, |config| {
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
    })
    .await;
    let result = mcp(
        &server,
        "lemonade_generate_image",
        json!({"prompt":"a lighthouse","size":"256x256","steps":2,"seed":5,"n":2}),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let blocks = result["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    for block in blocks {
        assert_eq!(block["type"], "image");
        assert_eq!(block["mimeType"], "image/png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(block["data"].as_str().unwrap())
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
    let disk = mcp(
        &server,
        "lemonade_generate_image",
        json!({"prompt":"a lighthouse","output_path":"cat.png"}),
    )
    .await;
    assert_eq!(disk["isError"], true);
}

const THINKING_TEMPLATE: &str = "{%- for message in messages %}<|im_start|>{{ message.role }}\n{{ message.content }}<|im_end|>\n{% endfor %}{%- if add_generation_prompt %}<|im_start|>assistant\n{%- if enable_thinking is defined and enable_thinking is false %}\n<think>\n\n</think>\n\n{%- endif %}{%- endif %}";

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; real llama-server chat template evidence"]
async fn real_llama_server_renders_thinking_off_for_every_protocol() {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    let directory = tempfile::tempdir().unwrap();
    let templated = directory.path().join("stories-thinking.gguf");
    std::fs::write(
        &templated,
        gguf::with_text(
            &std::fs::read(&model).unwrap(),
            "tokenizer.chat_template",
            THINKING_TEMPLATE,
        ),
    )
    .unwrap();
    let server = Server::configured(true, |config| {
        config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
        config["startup_timeout_ms"] = json!(30000);
        config["request_timeout_ms"] = json!(30000);
        config["models"] = json!([{"id":"stories","model_file":templated,"command":engine,"provider":"llama-cpp",
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size","512","--parallel","1","--threads","2","--slots","--n-gpu-layers","0"],
            "resident_bytes":512u64*1024*1024,"max_inflight":1,"operations":["chat/completions","completions"],"policy":"on-demand"}]);
    })
    .await;
    let messages = json!([{"role":"user","content":"Hi"}]);
    let openai = |extra: Value| {
        let mut body =
            json!({"model":"stories","messages":messages,"max_tokens":2,"temperature":0});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let mut prompt_tokens = Vec::new();
    for body in [
        openai(json!({})),
        openai(json!({"chat_template_kwargs":{"enable_thinking":false}})),
        openai(json!({"enable_thinking":false})),
    ] {
        let reply: Value = post(&server, "/v1/chat/completions", body)
            .await
            .json()
            .await
            .unwrap();
        prompt_tokens.push(
            reply["usage"]["prompt_tokens"]
                .as_u64()
                .unwrap_or_else(|| panic!("{reply}")),
        );
    }
    let [on, off, openai_off] = prompt_tokens.try_into().unwrap();
    assert!(
        off > on,
        "thinking off adds the empty think block: {off} vs {on}"
    );
    assert_eq!(openai_off, off);
    let anthropic: Value = post(
        &server,
        "/v1/messages",
        json!({"model":"stories","max_tokens":2,"messages":messages,"thinking":{"type":"disabled"},"metadata":{"user_id":"u"}}),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(anthropic["usage"]["input_tokens"], off, "{anthropic}");
    for (think, expected) in [(false, off), (true, on)] {
        let ollama: Value = post(
            &server,
            "/api/chat",
            json!({"model":"stories","messages":messages,"stream":false,"think":think,"options":{"num_predict":2,"min_p":0.05,"repeat_penalty":1.1}}),
        )
        .await
        .json()
        .await
        .unwrap();
        assert_eq!(ollama["prompt_eval_count"], expected, "{ollama}");
    }
    let tool = mcp(
        &server,
        "lemonade_chat",
        json!({"messages":messages,"max_tokens":2}),
    )
    .await;
    assert_eq!(
        tool["structuredContent"]["usage"]["prompt_tokens"], off,
        "{tool}"
    );
}
