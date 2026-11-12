use super::catalog::Engine;
use crate::profiles::{DeclaredContext, ModelShape};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub const DEFAULT_CONTEXT_TOKENS: u64 = 8192;
pub const UNKNOWN_CONTEXT_TOKENS: u64 = 4096;
pub const WORKSPACE_ALLOWANCE_BYTES: u64 = 256 * 1024 * 1024;
const KV_ELEMENT_BYTES: u64 = 2;
const WHISPER_MAGIC: u32 = 0x6767_6d6c;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "format", rename_all = "kebab-case")]
pub enum ModelFormat {
    Gguf {
        declared: DeclaredContext,
        shape: ModelShape,
    },
    WhisperGgml {
        vocabulary: u32,
        mel_bins: u32,
        text_layers: u32,
    },
}

impl ModelFormat {
    pub fn engine(&self) -> Engine {
        match self {
            Self::Gguf { .. } => Engine::LlamaCpp,
            Self::WhisperGgml { .. } => Engine::WhisperCpp,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    Unavailable,
    Unsupported,
    Invalid(&'static str),
}

pub fn detect(path: &Path) -> Result<(ModelFormat, u64), FormatError> {
    let mut file = File::open(path).map_err(|_| FormatError::Unavailable)?;
    let bytes = file.metadata().map_err(|_| FormatError::Unavailable)?.len();
    let mut magic = [0; 4];
    file.read_exact(&mut magic)
        .map_err(|_| FormatError::Unsupported)?;
    if &magic == b"GGUF" {
        file.seek(SeekFrom::Start(0))
            .map_err(|_| FormatError::Unavailable)?;
        let (declared, shape) = crate::profiles::read_gguf_shape(&mut file)
            .map_err(|error| FormatError::Invalid(error.code))?;
        return Ok((ModelFormat::Gguf { declared, shape }, bytes));
    }
    if u32::from_le_bytes(magic) == WHISPER_MAGIC {
        let mut header = [0; 44];
        file.read_exact(&mut header)
            .map_err(|_| FormatError::Unsupported)?;
        let field = |index: usize| {
            i32::from_le_bytes(header[index * 4..index * 4 + 4].try_into().expect("field"))
        };
        let (vocabulary, mel_bins, text_layers) = (field(0), field(9), field(8));
        if (50_000..=52_000).contains(&vocabulary)
            && matches!(mel_bins, 80 | 128)
            && (1..=64).contains(&text_layers)
            && (0..=10).all(|index| field(index) >= 0)
        {
            return Ok((
                ModelFormat::WhisperGgml {
                    vocabulary: vocabulary as u32,
                    mel_bins: mel_bins as u32,
                    text_layers: text_layers as u32,
                },
                bytes,
            ));
        }
    }
    Err(FormatError::Unsupported)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Estimate {
    pub file_bytes: u64,
    pub context_tokens: Option<u64>,
    pub kv_bytes: u64,
    pub workspace_bytes: u64,
    pub resident_bytes: u64,
    pub basis: &'static str,
    pub formula: &'static str,
}

/// KV bytes = layers x context x KV heads x (key + value head width) x 2 bytes (f16 cache).
/// Head widths default to embedding / heads and KV heads default to heads. When the shape is
/// incomplete, KV is charged as a quarter of the weight file instead.
pub fn estimate(format: &ModelFormat, file_bytes: u64, context: Option<u64>) -> Estimate {
    let (kv_bytes, basis) = match (format, context) {
        (ModelFormat::Gguf { shape, .. }, Some(context)) => match kv_bytes(shape, context) {
            Some(bytes) => (bytes, "gguf-shape"),
            None => (file_bytes / 4, "file-fraction"),
        },
        _ => (0, "weights-only"),
    };
    Estimate {
        file_bytes,
        context_tokens: context,
        kv_bytes,
        workspace_bytes: WORKSPACE_ALLOWANCE_BYTES,
        resident_bytes: file_bytes
            .saturating_add(kv_bytes)
            .saturating_add(WORKSPACE_ALLOWANCE_BYTES),
        basis,
        formula: "file_bytes + layers*context*kv_heads*(key_width+value_width)*2 + 256 MiB",
    }
}

fn kv_bytes(shape: &ModelShape, context: u64) -> Option<u64> {
    let layers = shape.block_count?;
    let heads = shape.head_count;
    let width = shape
        .embedding_length
        .zip(heads)
        .and_then(|(embedding, heads)| embedding.checked_div(heads));
    let key = shape.key_length.or(width)?;
    let value = shape.value_length.or(width)?;
    let kv_heads = shape.head_count_kv.or(heads)?;
    layers
        .checked_mul(context)?
        .checked_mul(kv_heads)?
        .checked_mul(key.checked_add(value)?)?
        .checked_mul(KV_ELEMENT_BYTES)
}

pub fn context_tokens(format: &ModelFormat) -> Option<u64> {
    match format {
        ModelFormat::Gguf { declared, .. } => Some(
            declared
                .training_context_tokens
                .map_or(UNKNOWN_CONTEXT_TOKENS, |tokens| {
                    tokens.min(DEFAULT_CONTEXT_TOKENS)
                }),
        ),
        ModelFormat::WhisperGgml { .. } => None,
    }
}

#[derive(Clone, Debug)]
pub struct ProfileInputs<'a> {
    pub id: &'a str,
    pub model_file: &'a Path,
    pub artifact: Option<&'a str>,
    pub artifact_set: Option<&'a str>,
    pub package: &'a str,
    pub gpu: bool,
    pub cpu_threads: Option<u64>,
    pub format: &'a ModelFormat,
    pub estimate: &'a Estimate,
}

pub fn registration(inputs: &ProfileInputs<'_>) -> Value {
    let engine = inputs.format.engine();
    let (args, options, operations) = match inputs.format {
        ModelFormat::Gguf { shape, .. } => {
            let options = if inputs.gpu {
                json!({
                    "context_tokens": inputs.estimate.context_tokens,
                    "gpu_layers": shape.block_count.map_or(999, |layers| layers.saturating_add(1).min(999)),
                    "parallel_slots": 1,
                })
            } else {
                let mut options = json!({
                    "context_tokens": inputs.estimate.context_tokens,
                    "gpu_layers": 0,
                    "parallel_slots": 1,
                });
                if let Some(threads) = inputs
                    .cpu_threads
                    .filter(|threads| (1..=65536).contains(threads))
                {
                    options["threads"] = json!(threads);
                }
                options
            };
            (
                json!([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "{port}",
                    "--model",
                    "{model}",
                    "--slots",
                    "--jinja"
                ]),
                options,
                json!(["chat/completions", "completions", "responses"]),
            )
        }
        ModelFormat::WhisperGgml { .. } => (
            json!([
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--model",
                "{model}"
            ]),
            json!({}),
            json!(["audio/transcriptions"]),
        ),
    };
    let mut registration = json!({
        "id": inputs.id,
        "model_file": inputs.model_file,
        "execution_profile": {
            "schema_version": 1,
            "adapter": engine.provider(),
            "implementation_id": engine.provider().implementation_id(),
            "runtime_package": inputs.package,
            "args": args,
            "options": options,
            "placement": "unresolved",
        },
        "resident_bytes": inputs.estimate.resident_bytes,
        "max_inflight": 1,
        "operations": operations,
        "policy": "disabled",
    });
    if let Some(artifact) = inputs.artifact {
        registration["artifact"] = json!(artifact);
    }
    if let Some(set) = inputs.artifact_set {
        registration["artifact_set"] = json!(set);
    }
    registration
}

pub fn default_id(name: &str) -> Option<String> {
    let file = name.rsplit(['/', '\\']).next()?;
    let stem = file
        .strip_suffix(".gguf")
        .or_else(|| file.strip_suffix(".bin"))
        .unwrap_or(file);
    let id: String = stem
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect();
    let id = id.trim().to_owned();
    (!id.is_empty() && !id.contains(',')).then_some(id)
}
