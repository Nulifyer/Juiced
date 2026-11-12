//! FastFlowLM (`flm`) worker contract.
//!
//! FLM owns its model store: `flm pull <checkpoint>` downloads into `FLM_MODEL_PATH` and
//! `flm serve <checkpoint>` resolves the checkpoint there. Juiced keeps that ownership
//! instead of importing FLM weights into its artifact store, and binds a registration to
//! the checkpoint's `config.json` so launch identity still covers the files FLM loads.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const SERVE: &str = "serve";
pub const MODEL_PATH_ENV: &str = "FLM_MODEL_PATH";
pub const STORE_DIRECTORY: &str = "flm";
const MAX_CHECKPOINT: usize = 128;
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODELS: usize = 1024;

pub fn valid_checkpoint(value: &str) -> bool {
    let token = |part: &str| {
        !part.is_empty()
            && part.as_bytes()[0].is_ascii_alphanumeric()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    value.len() <= MAX_CHECKPOINT
        && value
            .split_once(':')
            .is_some_and(|(name, tag)| token(name) && token(tag))
}

/// The checkpoint a validated FLM template serves; FLM resolves requests by this name
/// rather than by the Juiced model ID.
pub fn checkpoint(arguments: &[String]) -> Option<&str> {
    match arguments {
        [serve, checkpoint, ..] if serve == SERVE && valid_checkpoint(checkpoint) => {
            Some(checkpoint)
        }
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Arity {
    Flag,
    Value,
}

/// Mirrors the native `flm_args` allow-list plus the context flag native load passes.
pub fn argument(flag: &str, value: Option<&str>) -> Result<Arity, &'static str> {
    let integer = |value: Option<&str>| value.and_then(|value| value.parse::<i64>().ok());
    let valid = match flag {
        "--quiet" => return Ok(Arity::Flag),
        "--ctx-len" => integer(value).is_some_and(|value| (1..=i32::MAX as i64).contains(&value)),
        "--pmode" => matches!(
            value,
            Some("default" | "powersaver" | "balanced" | "performance" | "turbo")
        ),
        "--prefill-chunk-len" => integer(value).is_some_and(|value| value == -1 || value >= 1),
        "--img-pre-resize" => integer(value).is_some_and(|value| (0..=4).contains(&value)),
        "--socket" | "--q-len" => integer(value).is_some_and(|value| (1..=1024).contains(&value)),
        "--preemption" => matches!(value, Some("0" | "1")),
        _ => return Err("unsupported_runtime_argument"),
    };
    if valid {
        Ok(Arity::Value)
    } else {
        Err("invalid_runtime_arguments")
    }
}

pub fn with_model(body: &[u8], checkpoint: &str) -> Option<axum::body::Bytes> {
    let mut document: serde_json::Map<String, Value> = serde_json::from_slice(body).ok()?;
    document.insert("model".into(), Value::String(checkpoint.to_owned()));
    serde_json::to_vec(&document)
        .ok()
        .map(axum::body::Bytes::from)
}

/// What the release's `flm` wrapper exports before exec'ing `flm-real`: XRT >= 2.25 derives
/// its root from where the loader found libxrt_coreutil, so the multiarch mirror resolves
/// first.
pub fn runtime_environment(root: &Path) -> Vec<(String, String)> {
    let path = |relative: &str| root.join(relative).to_string_lossy().into_owned();
    vec![
        (
            "LD_LIBRARY_PATH".into(),
            [path("lib/x86_64-linux-gnu"), path("lib")].join(":"),
        ),
        ("XILINX_XRT".into(), path("")),
    ]
}

pub fn store(data_directory: &Path) -> PathBuf {
    data_directory.join(STORE_DIRECTORY)
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ListedModel {
    pub checkpoint: String,
    pub installed: bool,
    pub footprint_gb: Option<f64>,
    pub labels: Vec<String>,
    pub kind: &'static str,
    #[serde(skip)]
    pub repo_directory: Option<String>,
}

/// Classifies like native discovery: FLM reports no deployment mode, so whisper and
/// embedding checkpoints are recognized by name and everything else is chat.
fn kind(checkpoint: &str) -> &'static str {
    let lower = checkpoint.to_ascii_lowercase();
    if lower.contains("whisper") {
        "transcription"
    } else if lower.contains("embed") {
        "embeddings"
    } else {
        "chat"
    }
}

pub fn repo_directory(url: &str) -> Option<String> {
    let mut clean = url.trim_end_matches('/');
    if let Some(index) = clean.find(['?', '#']) {
        clean = &clean[..index];
    }
    for marker in ["/tree/", "/resolve/"] {
        if let Some(index) = clean.find(marker) {
            clean = &clean[..index];
            break;
        }
    }
    let name = clean.rsplit('/').next()?;
    (!name.is_empty()
        && name != ".."
        && name != "."
        && name.len() <= 256
        && !name.contains(['/', '\\', '\0']))
    .then(|| name.to_owned())
}

pub fn parse_list(output: &str) -> Result<Vec<ListedModel>, &'static str> {
    if output.len() > MAX_LIST_BYTES {
        return Err("flm_output_invalid");
    }
    let document: Value = serde_json::from_str(output).map_err(|_| "flm_output_invalid")?;
    let models = document["models"].as_array().ok_or("flm_output_invalid")?;
    let mut listed = Vec::new();
    for model in models.iter().take(MAX_MODELS) {
        let Some(checkpoint) = model["name"].as_str().filter(|name| valid_checkpoint(name)) else {
            continue;
        };
        listed.push(ListedModel {
            checkpoint: checkpoint.to_owned(),
            installed: model["installed"] == true,
            footprint_gb: model["footprint"]
                .as_f64()
                .filter(|value| value.is_finite()),
            labels: model["label"]
                .as_array()
                .map(|labels| {
                    labels
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|label| label.len() <= 64)
                        .take(16)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            kind: kind(checkpoint),
            repo_directory: model["url"].as_str().and_then(repo_directory),
        });
    }
    Ok(listed)
}

/// Native resolves model files under `$FLM_MODEL_PATH/models/<repo>/config.json`.
pub fn config_path(store: &Path, repo_directory: &str) -> PathBuf {
    store
        .join("models")
        .join(repo_directory)
        .join("config.json")
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Validation {
    pub ready: Option<bool>,
    pub problems: Vec<&'static str>,
}

/// Interprets `flm validate --json` with the native field names and messages.
pub fn parse_validation(output: &str) -> Validation {
    let Ok(document) = serde_json::from_str::<Value>(output) else {
        return Validation::default();
    };
    let mut problems = Vec::new();
    for (field, message) in [
        ("amd_device_found", "No AMD NPU device found."),
        ("all_fw_ok", "NPU firmware is incompatible."),
        ("kernel_ok", "Kernel version is incompatible."),
        ("memlock_ok", "Memlock limits are too low."),
        ("npu_driver_ok", "NPU driver version is too old."),
    ] {
        if document[field] == false {
            problems.push(message);
        }
    }
    Validation {
        ready: document["ready"].as_bool(),
        problems,
    }
}

/// `[FLM]  Overall progress: 42.0% (1/3 files)`
pub fn progress_percent(line: &str) -> Option<u8> {
    let rest = line.split("Overall progress: ").nth(1)?;
    let value: f64 = rest.split('%').next()?.trim().parse().ok()?;
    (0.0..=100.0).contains(&value).then(|| value.floor() as u8)
}

pub fn pull_arguments(checkpoint: &str, force: bool) -> Vec<String> {
    let mut arguments = vec!["pull".to_owned(), checkpoint.to_owned()];
    if force {
        arguments.push("--force".into());
    }
    arguments
}

pub fn default_id(checkpoint: &str) -> String {
    format!("{}-FLM", checkpoint.replace(':', "-"))
}

/// Without an explicit context FLM applies its own per-model default.
pub fn serve_arguments(checkpoint: &str, context_tokens: Option<u32>) -> Vec<String> {
    let mut arguments: Vec<String> = vec![SERVE.into(), checkpoint.into()];
    if let Some(context) = context_tokens {
        arguments.extend(["--ctx-len".into(), context.to_string()]);
    }
    arguments.extend(["--port", "{port}", "--host", "127.0.0.1", "--quiet"].map(str::to_owned));
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_are_bounded_name_tag_literals() {
        for valid in ["gemma3:4b", "llama3.2:1b", "gpt-oss:20b", "qwen3.5:0.8b"] {
            assert!(valid_checkpoint(valid), "{valid}");
        }
        for invalid in [
            "", "gemma3", ":4b", "gemma3:", "../x:1", "a:b:c", "a b:1", "-x:1",
        ] {
            assert!(!valid_checkpoint(invalid), "{invalid}");
        }
        assert!(!valid_checkpoint(&format!("{}:1", "a".repeat(200))));
    }

    #[test]
    fn arguments_follow_the_native_allow_list() {
        assert_eq!(argument("--quiet", None), Ok(Arity::Flag));
        assert_eq!(argument("--pmode", Some("turbo")), Ok(Arity::Value));
        assert_eq!(
            argument("--prefill-chunk-len", Some("-1")),
            Ok(Arity::Value)
        );
        assert_eq!(argument("--img-pre-resize", Some("4")), Ok(Arity::Value));
        assert_eq!(argument("--q-len", Some("1024")), Ok(Arity::Value));
        for (flag, value) in [
            ("--pmode", "fast"),
            ("--prefill-chunk-len", "0"),
            ("--img-pre-resize", "5"),
            ("--socket", "0"),
            ("--preemption", "true"),
            ("--ctx-len", "0"),
        ] {
            assert_eq!(
                argument(flag, Some(value)),
                Err("invalid_runtime_arguments"),
                "{flag} {value}"
            );
        }
        assert_eq!(
            argument("--model-path", Some("/x")),
            Err("unsupported_runtime_argument")
        );
    }

    #[test]
    fn list_output_parses_like_native_discovery() {
        let listed = parse_list(
            r#"{"models":[
                {"name":"gemma3:4b","installed":true,"footprint":3.2,"label":["vision"],"url":"https://huggingface.co/FastFlowLM/Gemma3-4B-NPU2/tree/main"},
                {"name":"whisper-v3:turbo","installed":false},
                {"name":"embed-gemma:300m","installed":false},
                {"name":"../../etc:x"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].kind, "chat");
        assert_eq!(listed[0].repo_directory.as_deref(), Some("Gemma3-4B-NPU2"));
        assert_eq!(listed[0].footprint_gb, Some(3.2));
        assert_eq!(listed[1].kind, "transcription");
        assert_eq!(listed[2].kind, "embeddings");
        assert!(parse_list("not json").is_err());
        assert_eq!(repo_directory("https://x/a/.."), None);
    }

    #[test]
    fn validation_and_progress_use_native_fields() {
        let validation = parse_validation(
            r#"{"ready":false,"amd_device_found":true,"memlock_ok":false,"npu_driver_ok":false}"#,
        );
        assert_eq!(validation.ready, Some(false));
        assert_eq!(
            validation.problems,
            [
                "Memlock limits are too low.",
                "NPU driver version is too old."
            ]
        );
        assert_eq!(parse_validation("x"), Validation::default());
        assert_eq!(
            progress_percent("[FLM]  Overall progress: 42.7% (1/3 files)"),
            Some(42)
        );
        assert_eq!(progress_percent("[FLM]  Downloading: 3%"), None);
        assert_eq!(
            pull_arguments("gemma3:4b", true),
            ["pull", "gemma3:4b", "--force"]
        );
        assert_eq!(default_id("llama3.2:1b"), "llama3.2-1b-FLM");
    }

    #[test]
    fn worker_templates_bind_only_the_port_and_serve_a_literal_checkpoint() {
        use crate::{
            config::Provider,
            runtime_arguments::{Bindings, validate},
        };
        let template = validate(
            Provider::FastFlowLm,
            &serve_arguments("gemma3:4b", Some(4096)),
        )
        .unwrap();
        assert_eq!(
            template
                .resolve(Bindings {
                    model: "/data/flm/models/Gemma3-4B-NPU2/config.json",
                    port: 4321,
                    cache: "",
                    members: &[],
                })
                .unwrap(),
            [
                "serve",
                "gemma3:4b",
                "--ctx-len",
                "4096",
                "--port",
                "4321",
                "--host",
                "127.0.0.1",
                "--quiet"
            ]
        );
        let defaulted = validate(
            Provider::FastFlowLm,
            &["serve", "gemma3:4b", "--port", "{port}", "--pmode", "turbo"].map(str::to_owned),
        )
        .unwrap();
        assert_eq!(defaulted.arguments().last().unwrap(), "127.0.0.1");
        for (arguments, code) in [
            (
                &["gemma3:4b", "--port", "{port}"][..],
                "missing_runtime_binding",
            ),
            (
                &["serve", "../x", "--port", "{port}"],
                "missing_runtime_binding",
            ),
            (&["serve", "gemma3:4b"], "missing_runtime_binding"),
            (
                &["serve", "gemma3:4b", "--port", "8080"],
                "invalid_runtime_binding",
            ),
            (
                &[
                    "serve",
                    "gemma3:4b",
                    "--port",
                    "{port}",
                    "--host",
                    "0.0.0.0",
                ],
                "invalid_runtime_binding",
            ),
            (
                &[
                    "serve",
                    "gemma3:4b",
                    "--port",
                    "{port}",
                    "--quiet",
                    "--quiet",
                ],
                "duplicate_runtime_binding",
            ),
            (
                &["serve", "gemma3:4b", "--port", "{port}", "--embed", "1"],
                "unsupported_runtime_argument",
            ),
            (
                &["serve", "gemma3:4b", "--port", "{port}", "--pmode", "max"],
                "invalid_runtime_arguments",
            ),
        ] {
            let arguments: Vec<String> =
                arguments.iter().map(|value| (*value).to_owned()).collect();
            assert_eq!(
                validate(Provider::FastFlowLm, &arguments).unwrap_err().code,
                code,
                "{arguments:?}"
            );
        }
        assert_eq!(
            checkpoint(&serve_arguments("qwen3:8b", None)),
            Some("qwen3:8b")
        );
        assert_eq!(
            with_model(br#"{"model":"public-id","stream":true}"#, "qwen3:8b").unwrap(),
            br#"{"model":"qwen3:8b","stream":true}"#.as_slice()
        );
        assert!(with_model(b"[]", "qwen3:8b").is_none());
    }
}
