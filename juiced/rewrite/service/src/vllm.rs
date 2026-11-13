//! vLLM and DwarfStar4 worker contracts.
//!
//! vLLM is a Python engine. It runs from an engine environment's own interpreter as
//! `python -I -B -m vllm.entrypoints.cli.main serve <model directory>`. `-I` keeps the
//! service's Python settings out of the worker, and `-B` keeps the read-only environment
//! byte-identical to what was installed. The model directory holds an original
//! Transformers checkpoint (`config.json`, safetensors and tokenizer files).

use crate::runtime_arguments::Error;
use serde_json::Value;
use std::path::Path;

pub const MODULE: &str = "vllm.entrypoints.cli.main";
const PREFIX: [&str; 6] = ["-I", "-B", "-m", MODULE, "serve", "{model}"];
const MODEL_POSITION: usize = 5;
/// The first start compiles kernels for the model shape; later starts reuse the cache.
pub const STARTUP: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Weights, activations, the CUDA graph pool and the PyTorch context beyond the KV cache.
pub const RUNTIME_OVERHEAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_SEQUENCES: u32 = 4;
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

fn invalid() -> Error {
    Error::new(
        "invalid_runtime_arguments",
        "The vLLM worker accepts only its reviewed serve flags with valid values.",
    )
}

enum Arity {
    Flag,
    Value(fn(&str) -> bool),
}

fn positive(value: &str) -> bool {
    value.parse::<u64>().is_ok_and(|value| value > 0)
}

fn fraction(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|value| value > 0.0 && value <= 1.0)
}

fn number(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|value| value.is_finite() && value >= 0.0)
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn bytes(value: &str) -> bool {
    let digits = value.trim_end_matches(['B', 'i', 'K', 'M', 'G', 'T', 'k', 'm', 'g', 't']);
    !digits.is_empty()
        && value.len() - digits.len() <= 3
        && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn json_object(value: &str) -> bool {
    value.len() <= 16 * 1024 && serde_json::from_str::<Value>(value).is_ok_and(|v| v.is_object())
}

fn literal(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// The reviewed `vllm serve` flags. Flags that name files, networks, credentials or remote
/// code (`--download-dir`, `--chat-template`, `--api-key`, `--trust-remote-code`, ...)
/// are absent on purpose.
fn arity(flag: &str) -> Option<Arity> {
    Some(match flag {
        "--enforce-eager"
        | "--enable-auto-tool-choice"
        | "--enable-prefix-caching"
        | "--no-enable-prefix-caching"
        | "--enable-chunked-prefill"
        | "--no-enable-chunked-prefill"
        | "--disable-log-stats"
        | "--language-model-only"
        | "--async-scheduling"
        | "--no-async-scheduling" => Arity::Flag,
        "--max-model-len"
        | "--max-num-seqs"
        | "--max-num-batched-tokens"
        | "--max-logprobs"
        | "--block-size"
        | "--tensor-parallel-size"
        | "--pipeline-parallel-size" => Arity::Value(positive),
        "--seed" => Arity::Value(|value| value.parse::<u64>().is_ok()),
        "--gpu-memory-utilization" => Arity::Value(fraction),
        "--kv-cache-memory-bytes" => Arity::Value(bytes),
        "--swap-space" | "--cpu-offload-gb" => Arity::Value(number),
        "--dtype" => Arity::Value(|value| {
            matches!(
                value,
                "auto" | "half" | "float16" | "bfloat16" | "float" | "float32"
            )
        }),
        "--kv-cache-dtype" => Arity::Value(|value| {
            matches!(
                value,
                "auto" | "fp8" | "fp8_e4m3" | "fp8_e5m2" | "bfloat16" | "float16"
            )
        }),
        "--generation-config" => Arity::Value(|value| matches!(value, "auto" | "vllm")),
        "--quantization" | "--tool-call-parser" | "--reasoning-parser" | "--tokenizer-mode" => {
            Arity::Value(token)
        }
        "--speculative-config"
        | "--default-chat-template-kwargs"
        | "--limit-mm-per-prompt"
        | "--override-generation-config" => Arity::Value(json_object),
        "--served-model-name" => Arity::Value(literal),
        _ => return None,
    })
}

/// Validates a vLLM template and returns it with the model, port and host binding
/// positions (`[model, port, host, cache]`).
pub fn validate(arguments: &[String]) -> Result<(Vec<String>, [Option<usize>; 4]), Error> {
    if arguments.len() < PREFIX.len()
        || arguments
            .iter()
            .zip(PREFIX)
            .any(|(argument, expected)| argument != expected)
    {
        return Err(Error::new(
            "missing_runtime_binding",
            "The vLLM worker needs `-I -B -m vllm.entrypoints.cli.main serve {model}`.",
        ));
    }
    let mut result: Vec<String> = PREFIX.iter().map(|value| (*value).to_owned()).collect();
    let mut bindings = [Some(MODEL_POSITION), None, None, None];
    let mut seen = std::collections::BTreeSet::new();
    let mut position = PREFIX.len();
    while let Some(argument) = arguments.get(position) {
        let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        if !seen.insert(flag.to_owned()) {
            return Err(Error::new(
                "duplicate_runtime_binding",
                "A vLLM worker argument was supplied more than once.",
            ));
        }
        let value = || {
            inline
                .or_else(|| arguments.get(position + 1).map(String::as_str))
                .ok_or_else(invalid)
        };
        let step = if inline.is_some() { 1 } else { 2 };
        match flag {
            "--port" | "--host" => {
                let expected = if flag == "--port" {
                    "{port}"
                } else {
                    "127.0.0.1"
                };
                if value()? != expected {
                    return Err(Error::new(
                        "invalid_runtime_binding",
                        "Worker model, port, host and cache arguments must use their exact manager-owned bindings.",
                    ));
                }
                bindings[if flag == "--port" { 1 } else { 2 }] = Some(result.len() + 1);
                result.extend([flag.to_owned(), expected.to_owned()]);
                position += step;
            }
            _ => match arity(flag) {
                Some(Arity::Flag) if inline.is_none() => {
                    result.push(flag.to_owned());
                    position += 1;
                }
                Some(Arity::Value(valid)) => {
                    let value = value()?;
                    if !valid(value) {
                        return Err(invalid());
                    }
                    result.extend([flag.to_owned(), value.to_owned()]);
                    position += step;
                }
                Some(Arity::Flag) => return Err(invalid()),
                None => {
                    return Err(Error::new(
                        "unsupported_runtime_argument",
                        "The selected adapter does not define this worker argument or positional form.",
                    ));
                }
            },
        }
    }
    if bindings[1].is_none() {
        return Err(Error::new(
            "missing_runtime_binding",
            "The selected worker needs one model binding, one port binding and its required loopback host.",
        ));
    }
    if bindings[2].is_none() {
        bindings[2] = Some(result.len() + 1);
        result.extend(["--host".into(), "127.0.0.1".into()]);
    }
    Ok((result, bindings))
}

/// `ds4-server` flags the native `ds4_args` accepted, minus backend selection (only the
/// ROCm build is published) and the flags that write files or open other listeners.
pub fn ds4_arity(flag: &str) -> Option<u8> {
    match flag {
        "--ssd-streaming" | "--ssd-streaming-cold" => Some(0),
        "-c"
        | "--ctx"
        | "-n"
        | "--tokens"
        | "-t"
        | "--threads"
        | "--power"
        | "--ssd-streaming-cache-experts"
        | "--ssd-streaming-full-layers"
        | "--ssd-streaming-preload-experts"
        | "--prefill-chunk"
        | "--batched-session"
        | "--mixed-prefill-quantum" => Some(1),
        _ => None,
    }
}

/// What registration reads from a Transformers checkpoint's `config.json`.
#[derive(Clone, Debug, Default, serde::Serialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub architecture: String,
    pub context_tokens: Option<u64>,
    pub layers: Option<u64>,
    pub kv_heads: Option<u64>,
    pub head_dimension: Option<u64>,
    pub quantization: Option<String>,
    pub dtype: Option<String>,
    pub weights_bytes: u64,
    pub weight_files: u32,
}

fn field(config: &Value, name: &str) -> Option<u64> {
    config
        .get(name)
        .or_else(|| config.get("text_config").and_then(|text| text.get(name)))
        .and_then(Value::as_u64)
}

/// Reads a checkpoint directory: a bounded `config.json` that names an architecture, and
/// at least one safetensors weight file beside it.
pub fn checkpoint(config_path: &Path) -> Option<Checkpoint> {
    let directory = config_path.parent()?;
    let file = std::fs::File::open(config_path).ok()?;
    if file.metadata().ok()?.len() > MAX_CONFIG_BYTES {
        return None;
    }
    let config: Value = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
    let architecture = config
        .get("architectures")?
        .as_array()?
        .first()?
        .as_str()?
        .to_owned();
    let mut weights_bytes = 0u64;
    let mut weight_files = 0u32;
    for entry in std::fs::read_dir(directory).ok()?.flatten().take(4096) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".safetensors") {
            weights_bytes =
                weights_bytes.saturating_add(std::fs::metadata(entry.path()).ok()?.len());
            weight_files += 1;
        }
    }
    if weight_files == 0 {
        return None;
    }
    let attention_heads = field(&config, "num_attention_heads");
    let hidden = field(&config, "hidden_size");
    Some(Checkpoint {
        architecture,
        context_tokens: field(&config, "max_position_embeddings"),
        layers: field(&config, "num_hidden_layers"),
        kv_heads: field(&config, "num_key_value_heads").or(attention_heads),
        head_dimension: field(&config, "head_dim").or_else(|| Some(hidden? / attention_heads?)),
        quantization: config
            .get("quantization_config")
            .and_then(|quantization| quantization.get("quant_method"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        dtype: config
            .get("torch_dtype")
            .or_else(|| config.get("dtype"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        weights_bytes,
        weight_files,
    })
}

impl Checkpoint {
    /// Bytes per token of KV cache at 16-bit precision, when the config declares the shape.
    pub fn kv_bytes_per_token(&self) -> Option<u64> {
        Some(2 * 2 * self.layers? * self.kv_heads? * self.head_dimension?)
    }
}

/// The resident budget and the KV pool vLLM is given with `--kv-cache-memory-bytes`.
pub struct Memory {
    pub resident_bytes: u64,
    pub kv_bytes: u64,
}

pub fn memory(checkpoint: &Checkpoint, context: u64) -> Memory {
    // vLLM rounds the pool to whole blocks per layer and refuses to start when one
    // full-context request does not fit, so the budget carries a tenth plus 64 MiB spare.
    let kv_bytes = checkpoint
        .kv_bytes_per_token()
        .map_or(512 * 1024 * 1024, |per_token| {
            let exact = per_token.saturating_mul(context);
            exact.saturating_add(exact / 10)
        })
        .saturating_add(64 * 1024 * 1024);
    let resident_bytes = checkpoint
        .weights_bytes
        .saturating_add(kv_bytes)
        .saturating_add(RUNTIME_OVERHEAD_BYTES);
    Memory {
        resident_bytes,
        kv_bytes,
    }
}

/// The context a new registration starts with: the model's declared window, capped so the
/// first load stays small.
pub fn default_context(checkpoint: &Checkpoint) -> u64 {
    checkpoint
        .context_tokens
        .unwrap_or(crate::engines::models::DEFAULT_CONTEXT_TOKENS)
        .min(crate::engines::models::DEFAULT_CONTEXT_TOKENS)
}

/// Native per-family serve flags (v0.9.2 `vllm_model_config.json`).
pub fn family_arguments(architecture: &str, repository: Option<&str>) -> Vec<String> {
    let repository = repository.unwrap_or_default();
    if repository.contains("Qwen3.") || architecture.starts_with("Qwen3_5") {
        return [
            "--enable-auto-tool-choice",
            "--tool-call-parser",
            "qwen3_coder",
        ]
        .map(str::to_owned)
        .to_vec();
    }
    if repository.contains("GLM-4.7") {
        return ["--enable-auto-tool-choice", "--tool-call-parser", "glm47"]
            .map(str::to_owned)
            .to_vec();
    }
    Vec::new()
}

/// `--max-model-len` of a validated template.
pub fn max_model_len(arguments: &[String]) -> Option<u64> {
    arguments
        .iter()
        .position(|argument| argument == "--max-model-len")
        .and_then(|position| arguments.get(position + 1))
        .and_then(|value| value.parse().ok())
}

/// Requests above this many output tokens are fitted to the context before dispatch.
const MAX_TOKENS_PREFLIGHT: u64 = 8192;

/// vLLM rejects a request whose prompt plus `max_tokens` exceeds `--max-model-len`, where
/// llama.cpp would stop at the context end. Oversized limits are reduced to what the prompt
/// leaves, using the worker's own `/tokenize`, and streams always report usage so request
/// accounting has token counts.
pub async fn prepare_request(
    client: &reqwest::Client,
    base: &str,
    body: &[u8],
    streaming_usage: bool,
    context: Option<u64>,
) -> Option<Vec<u8>> {
    let mut request: serde_json::Map<String, Value> = serde_json::from_slice(body).ok()?;
    let mut changed = false;
    if streaming_usage && request.get("stream") == Some(&Value::Bool(true)) {
        let options = request
            .entry("stream_options")
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(options) = options.as_object_mut()
            && options.get("include_usage") != Some(&Value::Bool(true))
        {
            options.insert("include_usage".into(), Value::Bool(true));
            changed = true;
        }
    }
    let requested = ["max_completion_tokens", "max_tokens"]
        .iter()
        .filter_map(|key| request.get(*key).and_then(Value::as_u64))
        .max();
    if let (Some(context), Some(requested)) = (context, requested)
        && requested > MAX_TOKENS_PREFLIGHT.min(context)
    {
        let mut tokenize = serde_json::Map::new();
        for key in [
            "model",
            "messages",
            "prompt",
            "tools",
            "add_generation_prompt",
            "continue_final_message",
            "add_special_tokens",
            "chat_template_kwargs",
        ] {
            if let Some(value) = request.get(key) {
                tokenize.insert(key.into(), value.clone());
            }
        }
        let counted = client
            .post(format!("{base}/tokenize"))
            .json(&tokenize)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .ok()
            .filter(|response| response.status().is_success());
        let count = match counted {
            Some(response) => response
                .json::<Value>()
                .await
                .ok()
                .and_then(|value| value.get("count").and_then(Value::as_u64)),
            None => None,
        };
        if let Some(count) = count
            && count < context
            && requested > context - count
        {
            for key in ["max_completion_tokens", "max_tokens"] {
                if request.contains_key(key) {
                    request.insert(key.into(), Value::from(context - count));
                }
            }
            changed = true;
        }
    }
    changed.then(|| serde_json::to_vec(&request).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Vec<String> {
        PREFIX
            .iter()
            .chain(extra)
            .map(|value| (*value).to_owned())
            .collect()
    }

    #[test]
    fn template_binds_model_port_and_loopback_host() {
        let (resolved, bindings) =
            validate(&args(&["--port", "{port}", "--max-model-len", "4096"])).unwrap();
        assert_eq!(bindings[0], Some(5));
        assert_eq!(resolved[bindings[1].unwrap()], "{port}");
        assert_eq!(resolved[bindings[2].unwrap()], "127.0.0.1");
        assert_eq!(&resolved[resolved.len() - 2..], ["--host", "127.0.0.1"]);
    }

    #[test]
    fn template_rejects_unreviewed_paths_and_bad_values() {
        for extra in [
            &["--port", "{port}", "--trust-remote-code"][..],
            &["--port", "{port}", "--download-dir", "/tmp"],
            &["--port", "{port}", "--chat-template", "/etc/passwd"],
            &["--port", "{port}", "--gpu-memory-utilization", "1.5"],
            &["--port", "{port}", "--host", "0.0.0.0"],
            &["--port", "8000"],
            &["--port", "{port}", "--port", "{port}"],
            &["--max-model-len", "4096"],
        ] {
            assert!(validate(&args(extra)).is_err(), "{extra:?}");
        }
        assert!(validate(&["serve".to_owned(), "{model}".to_owned()]).is_err());
    }

    #[test]
    fn inline_values_and_json_flags_validate() {
        let (resolved, _) = validate(&args(&[
            "--port={port}",
            "--speculative-config={\"method\":\"mtp\",\"num_speculative_tokens\":1}",
            "--kv-cache-memory-bytes",
            "4G",
            "--enforce-eager",
        ]))
        .unwrap();
        assert!(resolved.contains(&"--enforce-eager".to_owned()));
        assert!(resolved.contains(&"4G".to_owned()));
        assert!(validate(&args(&["--port={port}", "--speculative-config", "[1]"])).is_err());
    }

    #[test]
    fn checkpoint_reads_shape_and_weights_and_estimates_memory() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("config.json"),
            r#"{"architectures":["Qwen3ForCausalLM"],"max_position_embeddings":40960,
               "num_hidden_layers":28,"num_attention_heads":16,"num_key_value_heads":8,
               "head_dim":128,"hidden_size":1024,"torch_dtype":"bfloat16"}"#,
        )
        .unwrap();
        assert!(checkpoint(&directory.path().join("config.json")).is_none());
        std::fs::write(directory.path().join("model.safetensors"), vec![0; 4096]).unwrap();
        let found = checkpoint(&directory.path().join("config.json")).unwrap();
        assert_eq!(found.architecture, "Qwen3ForCausalLM");
        assert_eq!(found.kv_bytes_per_token(), Some(2 * 2 * 28 * 8 * 128));
        assert_eq!(default_context(&found), 8192);
        let memory = memory(&found, 4096);
        let kv = 114_688 * 4096;
        assert_eq!(memory.kv_bytes, kv + kv / 10 + 64 * 1024 * 1024);
        assert_eq!(
            memory.resident_bytes,
            4096 + memory.kv_bytes + RUNTIME_OVERHEAD_BYTES
        );
    }

    #[test]
    fn ds4_accepts_streaming_flags_and_rejects_backend_and_file_flags() {
        assert_eq!(ds4_arity("--ssd-streaming"), Some(0));
        assert_eq!(ds4_arity("-c"), Some(1));
        for flag in [
            "--cpu",
            "--rocm",
            "--backend",
            "--trace",
            "--kv-disk-dir",
            "--cors",
        ] {
            assert_eq!(ds4_arity(flag), None, "{flag}");
        }
    }
}
