#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Real checkpoints pulled through Juiced from Hugging Face and served by real engines.
//! Opt in with `JUICED_TEST_REAL_CHECKPOINTS=<state directory>`; the directory keeps the
//! installed engines and downloads between runs (vLLM's environment is about 10 GB) and is
//! the caller's to delete. `JUICED_TEST_REAL_PORT` picks the first of two listener ports.

mod support;

use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Lemond {
    child: Child,
    base: String,
    client: reqwest::Client,
}

impl Drop for Lemond {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", self.child.id())])
            .status();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", self.child.id())])
            .status();
        let _ = self.child.wait();
    }
}

fn state(name: &str) -> Option<PathBuf> {
    let root = std::env::var_os("JUICED_TEST_REAL_CHECKPOINTS")?;
    let path = PathBuf::from(root).join(name);
    std::fs::create_dir_all(&path).unwrap();
    Some(path)
}

fn port(offset: u16) -> u16 {
    std::env::var("JUICED_TEST_REAL_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(13560)
        + offset
}

async fn start(directory: &Path, port: u16) -> Lemond {
    let config = directory.join("config.json");
    if !config.exists() {
        std::fs::write(
            &config,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1, "inhibit_suspend": false, "models": [],
                "resident_budget_bytes": 12u64 * 1024 * 1024 * 1024,
                "startup_timeout_ms": 900_000, "request_timeout_ms": 600_000,
                "session_cache": {"mode": "off"},
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let mut command = Command::new(support::binary("lemond", "JUICED_TEST_SERVER"));
    command
        .arg("--config")
        .arg(&config)
        .args(["--listen", &format!("127.0.0.1:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .env_remove("LEMONADE_API_KEY")
        .env_remove("LEMONADE_ADMIN_API_KEY")
        .env_remove("JUICED_HUGGINGFACE_URL");
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let lemond = Lemond {
        child: command.spawn().expect("start lemond"),
        base: format!("http://127.0.0.1:{port}"),
        client: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(900))
            .build()
            .unwrap(),
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while lemond
        .client
        .get(format!("{}/v1/health", lemond.base))
        .send()
        .await
        .is_err()
    {
        assert!(Instant::now() < deadline, "lemond did not start");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    lemond
}

impl Lemond {
    async fn get(&self, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{}/v1/{path}", self.base))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert!(status.is_success(), "GET {path}: {status} {body}");
        body
    }
    async fn post(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let response = self
            .client
            .post(format!("{}/v1/{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

async fn until(
    what: &str,
    limit: Duration,
    mut condition: impl AsyncFnMut() -> Option<Value>,
) -> Value {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = condition().await {
            return value;
        }
        assert!(Instant::now() < deadline, "{what} did not finish in time");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Searches for `repo` limited to `engine`, pulls its checkpoint and returns the pull view.
async fn acquire(lemond: &Lemond, repo: &str, engine: &str, query: &str) -> Value {
    let result = lemond
        .get(&format!("juiced/library/search?q={query}&engine={engine}"))
        .await;
    let entry = result["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["repo"] == repo)
        .unwrap_or_else(|| panic!("{repo} missing from {result}"))
        .clone();
    let quant = entry["quants"][0].clone();
    assert_eq!(quant["kind"], "checkpoint", "{entry}");
    assert_eq!(quant["engine"], engine, "{entry}");
    eprintln!(
        "{repo}: {} files, {} bytes",
        quant["files"].as_array().unwrap().len(),
        quant["bytes"]
    );
    let (status, started) = lemond
        .post(
            "juiced/library/pull",
            json!({"repo": repo, "file": quant["file"], "expected_generation": result["artifact_generation"]}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{started}");
    let id = started["pull"].as_str().unwrap().to_owned();
    let done = until(
        "checkpoint download",
        Duration::from_secs(3600),
        async || {
            let view = lemond.get(&format!("juiced/library/pulls/{id}")).await;
            matches!(view["state"].as_str(), Some("complete" | "failed")).then_some(view)
        },
    )
    .await;
    assert_eq!(done["state"], "complete", "{done}");
    done
}

async fn ensure_engine(lemond: &Lemond, engine: &str, backend: &str) {
    let engines = lemond.get("juiced/engines").await;
    let row = engines["installable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["engine"] == engine && row["backend"] == backend)
        .cloned()
        .unwrap_or_else(|| panic!("{engine} {backend} not offered: {engines}"));
    if row["installed_package"].is_string() {
        return;
    }
    let (status, accepted) = lemond
        .post(
            "juiced/engines/install",
            json!({"engine": engine, "backend": backend, "expected_generation": engines["generation"]}),
        )
        .await;
    assert!(status.is_success(), "{accepted}");
    if accepted["already_installed"] == true {
        return;
    }
    let id = accepted["job"]["id"].as_str().unwrap().to_owned();
    let job = until("engine install", Duration::from_secs(5400), async || {
        let engines = lemond.get("juiced/engines").await;
        engines["jobs"]
            .as_array()?
            .iter()
            .find(|job| {
                job["id"] == id.as_str()
                    && matches!(job["state"].as_str(), Some("complete" | "failed"))
            })
            .cloned()
    })
    .await;
    assert_eq!(job["state"], "complete", "{job}");
}

async fn add(lemond: &Lemond, set: &str) -> Value {
    let (status, added) = lemond
        .post(
            "juiced/models/add",
            json!({"artifact_set": set, "policy": "on-demand"}),
        )
        .await;
    if status == reqwest::StatusCode::CONFLICT && added["error"]["code"] == "model_exists" {
        return added;
    }
    assert_eq!(status, reqwest::StatusCode::OK, "{added}");
    eprintln!("registered: {}", added["execution_profile"]);
    added
}

#[tokio::test]
async fn vllm_cuda_serves_a_transformers_checkpoint_pulled_through_juiced() {
    let Some(directory) = state("vllm") else {
        return;
    };
    let lemond = start(&directory, port(0)).await;
    let done = acquire(&lemond, "Qwen/Qwen3-0.6B", "vllm", "Qwen3-0.6B").await;
    assert_eq!(done["format"], "transformers");
    ensure_engine(&lemond, "vllm", "cuda").await;
    let added = add(&lemond, done["set"].as_str().unwrap()).await;
    if added["engine"].is_string() {
        assert_eq!(added["engine"], "vllm");
        assert_eq!(added["backend"], "cuda");
        assert_eq!(added["model"]["id"], "Qwen3-0.6B");
    }
    let (status, reply) = lemond
        .post(
            "chat/completions",
            json!({"model": "Qwen3-0.6B", "max_tokens": 24, "chat_template_kwargs": {"enable_thinking": false},
                   "messages": [{"role": "user", "content": "Reply with the single word: ready"}]}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{reply}");
    let content = reply["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    eprintln!("vLLM replied: {content:?}");
    assert!(!content.trim().is_empty(), "{reply}");
}

#[tokio::test]
async fn ort_classifies_with_the_phishing_checkpoint_pulled_through_juiced() {
    let Some(directory) = state("onnx") else {
        return;
    };
    let lemond = start(&directory, port(1)).await;
    let done = acquire(
        &lemond,
        "lemonade-sdk/phishing-email-detection-distilbert-ONNX",
        "onnxruntime",
        "phishing-email-detection-distilbert",
    )
    .await;
    assert_eq!(done["format"], "onnx-classifier");
    ensure_engine(&lemond, "onnxruntime", "cpu").await;
    add(&lemond, done["set"].as_str().unwrap()).await;
    let model = "phishing-email-detection-distilbert-ONNX";
    let mut labels = Vec::new();
    for text in [
        "URGENT: your account is locked. Verify your password at http://secure-login.example now.",
        "Hi team, the meeting moved to 3pm tomorrow. See you there.",
    ] {
        let (status, reply) = lemond
            .post(
                "classify",
                json!({"model": model, "input": text, "top_k": 1}),
            )
            .await;
        assert_eq!(status, reqwest::StatusCode::OK, "{reply}");
        eprintln!("{text:?} -> {}", reply["labels"]);
        labels.push(reply["labels"].clone());
    }
    assert_ne!(
        labels[0], labels[1],
        "the two emails should classify differently"
    );
}
