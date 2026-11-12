use super::catalog::Engine;
use crate::{
    artifacts::ArtifactRole,
    profiles::{DeclaredContext, ModelShape},
};
use serde::{
    Deserialize, Serialize,
    de::{self, IgnoredAny, MapAccess, Visitor},
};
use serde_json::{Value, json};
use std::{
    fmt,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

pub const DEFAULT_CONTEXT_TOKENS: u64 = 8192;
pub const UNKNOWN_CONTEXT_TOKENS: u64 = 4096;
pub const WORKSPACE_ALLOWANCE_BYTES: u64 = 256 * 1024 * 1024;
pub const DIFFUSION_WORKSPACE_BYTES: u64 = 1024 * 1024 * 1024;
pub const SPEECH_WORKSPACE_BYTES: u64 = 512 * 1024 * 1024;
const KV_ELEMENT_BYTES: u64 = 2;
const WHISPER_MAGIC: u32 = 0x6767_6d6c;
const MAX_TENSOR_NAMES: u64 = 65_536;
const MAX_SAFETENSORS_HEADER: u64 = 64 * 1024 * 1024;
const MAX_VOICES: usize = 1024;
const ORT_IDENTIFIER: &[u8] = b"ORTM";
const MAX_SPEECH_CONFIG_BYTES: u64 = 64 * 1024;
const SPEECH_DEFAULT_THREADS: u64 = 4;
const PARAKEET_MAX_THREADS: u64 = 12;
const ASR4ALL_MAX_THREADS: u64 = 32;
const ASR4ALL_COMMIT_FRAMES: u32 = 256;
const ASR4ALL_RIGHT_FRAMES: u32 = 32;
const MOONSHINE_DEFAULT_ARCH: u32 = 5;
pub const MOONSHINE_CONFIG: &str = "streaming_config.json";
pub const MOONSHINE_TOKENIZER: &str = "tokenizer.json";

const DIFFUSION_ARCHITECTURES: &[&str] = &[
    "flux",
    "flux2",
    "sd1",
    "sd2",
    "sdxl",
    "sd3",
    "aura",
    "ltxv",
    "hyvid",
    "wan",
    "lumina2",
    "hidream",
    "cosmos",
    "qwen_image",
    "chroma",
    "z_image",
];
const DENOISER_PREFIXES: &[&str] = &[
    "model.diffusion_model.",
    "diffusion_model.",
    "double_blocks.",
    "single_blocks.",
    "joint_blocks.",
    "transformer_blocks.",
    "input_blocks.",
    "noise_refiner.",
    "context_refiner.",
];
const VAE_PREFIXES: &[&str] = &["first_stage_model.", "vae."];
const TEXT_ENCODER_PREFIXES: &[&str] = &[
    "cond_stage_model.",
    "conditioner.embedders.",
    "text_encoders.",
];

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Container {
    Gguf,
    Safetensors,
}

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
    Diffusion {
        container: Container,
        architecture: Option<String>,
        includes_vae: bool,
        includes_text_encoder: bool,
    },
    Onnx {
        ir_version: u32,
    },
    ParakeetGguf {},
    Asr4allGguf {},
    MoonshineOrt {
        model_arch: Option<u32>,
    },
}

impl ModelFormat {
    pub fn engine(&self) -> Engine {
        match self {
            Self::Gguf { .. } => Engine::LlamaCpp,
            Self::WhisperGgml { .. } => Engine::WhisperCpp,
            Self::Diffusion { .. } => Engine::StableDiffusionCpp,
            Self::Onnx { .. } => Engine::Kokoros,
            Self::ParakeetGguf { .. } => Engine::Parakeet,
            Self::Asr4allGguf { .. } => Engine::Asr4all,
            Self::MoonshineOrt { .. } => Engine::Moonshine,
        }
    }
    pub fn standalone(&self) -> bool {
        match self {
            Self::Diffusion {
                includes_vae,
                includes_text_encoder,
                ..
            } => *includes_vae && *includes_text_encoder,
            Self::Onnx { .. } | Self::MoonshineOrt { .. } => false,
            Self::Gguf { .. }
            | Self::WhisperGgml { .. }
            | Self::ParakeetGguf { .. }
            | Self::Asr4allGguf { .. } => true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    Unavailable,
    Unsupported,
    Invalid(&'static str),
}

#[derive(Default)]
struct TensorClasses {
    denoiser: bool,
    vae: bool,
    text_encoder: bool,
}

impl TensorClasses {
    fn observe(&mut self, name: &str) {
        let has = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));
        self.denoiser |= has(DENOISER_PREFIXES);
        self.vae |= has(VAE_PREFIXES);
        self.text_encoder |= has(TEXT_ENCODER_PREFIXES);
    }
}

pub fn detect(path: &Path) -> Result<(ModelFormat, u64), FormatError> {
    let mut file = File::open(path).map_err(|_| FormatError::Unavailable)?;
    let bytes = file.metadata().map_err(|_| FormatError::Unavailable)?.len();
    let mut magic = [0; 8];
    let available = read_prefix(&mut file, &mut magic)?;
    if available >= 4 && &magic[..4] == b"GGUF" {
        file.seek(SeekFrom::Start(0))
            .map_err(|_| FormatError::Unavailable)?;
        let (declared, shape) = crate::profiles::read_gguf_shape(&mut file)
            .map_err(|error| FormatError::Invalid(error.code))?;
        match declared.architecture.as_deref() {
            Some("parakeet") => return Ok((ModelFormat::ParakeetGguf {}, bytes)),
            Some("asr4all") => return Ok((ModelFormat::Asr4allGguf {}, bytes)),
            _ => {}
        }
        let mut classes = TensorClasses::default();
        let diffusion_architecture = declared
            .architecture
            .as_deref()
            .is_some_and(|architecture| DIFFUSION_ARCHITECTURES.contains(&architecture));
        if declared.architecture.is_none() || diffusion_architecture {
            crate::profiles::visit_gguf_tensor_names(&mut file, MAX_TENSOR_NAMES, |name| {
                classes.observe(name)
            })
            .map_err(|error| FormatError::Invalid(error.code))?;
        }
        if diffusion_architecture || classes.denoiser {
            return Ok((
                ModelFormat::Diffusion {
                    container: Container::Gguf,
                    architecture: declared.architecture,
                    includes_vae: classes.vae,
                    includes_text_encoder: classes.text_encoder,
                },
                bytes,
            ));
        }
        return Ok((ModelFormat::Gguf { declared, shape }, bytes));
    }
    if available >= 4 && u32::from_le_bytes(magic[..4].try_into().expect("magic")) == WHISPER_MAGIC
    {
        return whisper(&mut file, bytes);
    }
    if available == 8 && &magic[4..8] == ORT_IDENTIFIER {
        return Ok((ModelFormat::MoonshineOrt { model_arch: None }, bytes));
    }
    if let Some(ir_version) = onnx_version(&magic[..available]) {
        return Ok((ModelFormat::Onnx { ir_version }, bytes));
    }
    if available == 8 {
        let header = u64::from_le_bytes(magic);
        if (2..=MAX_SAFETENSORS_HEADER).contains(&header) && header < bytes {
            let classes = safetensors_classes(&mut file, header)?;
            if classes.denoiser {
                return Ok((
                    ModelFormat::Diffusion {
                        container: Container::Safetensors,
                        architecture: None,
                        includes_vae: classes.vae,
                        includes_text_encoder: classes.text_encoder,
                    },
                    bytes,
                ));
            }
        }
    }
    Err(FormatError::Unsupported)
}

/// Cheap first-bytes screen that [`detect`] would not reject outright, so uploads of
/// unsupported files stop before their remaining bytes are stored.
pub fn plausible_prefix(prefix: &[u8; 8], bytes: u64) -> bool {
    &prefix[..4] == b"GGUF"
        || u32::from_le_bytes(prefix[..4].try_into().expect("magic")) == WHISPER_MAGIC
        || onnx_version(prefix).is_some()
        || (2..=MAX_SAFETENSORS_HEADER.min(bytes.saturating_sub(1)))
            .contains(&u64::from_le_bytes(*prefix))
}

fn read_prefix(file: &mut File, buffer: &mut [u8]) -> Result<usize, FormatError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(FormatError::Unavailable),
        }
    }
    Ok(filled)
}

fn whisper(file: &mut File, bytes: u64) -> Result<(ModelFormat, u64), FormatError> {
    file.seek(SeekFrom::Start(4))
        .map_err(|_| FormatError::Unavailable)?;
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
    Err(FormatError::Unsupported)
}

/// ONNX `ModelProto` starts with field 1 (`ir_version`, varint) followed by another known
/// top-level field tag.
fn onnx_version(prefix: &[u8]) -> Option<u32> {
    if prefix.len() < 3 || prefix[0] != 0x08 || !(3..=12).contains(&prefix[1]) {
        return None;
    }
    matches!(
        prefix[2],
        0x12 | 0x1a | 0x22 | 0x28 | 0x32 | 0x3a | 0x42 | 0x72
    )
    .then_some(u32::from(prefix[1]))
}

fn safetensors_classes(file: &mut File, header: u64) -> Result<TensorClasses, FormatError> {
    struct Keys(TensorClasses);
    impl<'de> de::DeserializeSeed<'de> for &mut Keys {
        type Value = ();
        fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
            deserializer.deserialize_map(self)
        }
    }
    impl<'de> Visitor<'de> for &mut Keys {
        type Value = ();
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a safetensors header object")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            let mut entries = 0u64;
            while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                entries += 1;
                if entries > MAX_TENSOR_NAMES {
                    return Err(de::Error::custom("too many tensors"));
                }
                self.0.observe(&key);
                map.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }
    file.seek(SeekFrom::Start(8))
        .map_err(|_| FormatError::Unavailable)?;
    let mut keys = Keys(TensorClasses::default());
    let reader = BufReader::with_capacity(64 * 1024, file.take(header));
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    de::DeserializeSeed::deserialize(&mut keys, &mut deserializer)
        .map_err(|_| FormatError::Unsupported)?;
    Ok(keys.0)
}

/// Kokoro voice packs are NumPy `.npz` archives with one `<voice>.npy` entry per voice.
pub fn voices(path: &Path) -> Result<Vec<String>, FormatError> {
    let file = File::open(path).map_err(|_| FormatError::Unavailable)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| FormatError::Unsupported)?;
    if archive.is_empty() || archive.len() > MAX_VOICES {
        return Err(FormatError::Invalid("voice_pack_limit"));
    }
    let mut names = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|_| FormatError::Unsupported)?;
        let raw = entry.name().map_err(|_| FormatError::Unsupported)?;
        let name = raw
            .strip_suffix(".npy")
            .filter(|name| valid_voice(name))
            .ok_or(FormatError::Invalid("invalid_voice_pack"))?;
        names.push(name.to_owned());
    }
    names.sort();
    names.dedup();
    Ok(names)
}

pub fn valid_voice(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
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
/// incomplete, KV is charged as a quarter of the weight file instead. Image and speech
/// workers charge their weights, companions and a fixed per-engine workspace.
pub fn estimate(format: &ModelFormat, file_bytes: u64, context: Option<u64>) -> Estimate {
    match format {
        ModelFormat::Diffusion { .. }
        | ModelFormat::Onnx { .. }
        | ModelFormat::ParakeetGguf {}
        | ModelFormat::Asr4allGguf {}
        | ModelFormat::MoonshineOrt { .. } => {
            let workspace = if matches!(format, ModelFormat::Diffusion { .. }) {
                DIFFUSION_WORKSPACE_BYTES
            } else {
                SPEECH_WORKSPACE_BYTES
            };
            return Estimate {
                file_bytes,
                context_tokens: None,
                kv_bytes: 0,
                workspace_bytes: workspace,
                resident_bytes: file_bytes.saturating_add(workspace),
                basis: "weights-plus-workspace",
                formula: "file_bytes (including companions) + fixed engine workspace",
            };
        }
        _ => {}
    }
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

pub fn kv_bytes(shape: &ModelShape, context: u64) -> Option<u64> {
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
        ModelFormat::WhisperGgml { .. }
        | ModelFormat::Diffusion { .. }
        | ModelFormat::Onnx { .. }
        | ModelFormat::ParakeetGguf {}
        | ModelFormat::Asr4allGguf {}
        | ModelFormat::MoonshineOrt { .. } => None,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Companion {
    pub name: String,
    pub role: ArtifactRole,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompanionError {
    Required(&'static str),
    Unsupported,
}

/// Maps retained companions to worker arguments; every companion must be consumed.
pub fn companion_arguments(
    format: &ModelFormat,
    companions: &[Companion],
) -> Result<Vec<String>, CompanionError> {
    let placeholder = crate::runtime_arguments::member_placeholder;
    match format {
        ModelFormat::Onnx { .. } => match companions {
            [voice] if voice.role == ArtifactRole::Voice => {
                Ok(vec!["--data".into(), placeholder(&voice.name)])
            }
            [] => Err(CompanionError::Required("kokoro_voices_required")),
            _ => Err(CompanionError::Unsupported),
        },
        ModelFormat::Diffusion { .. } => {
            let mut arguments = Vec::new();
            let mut flags: Vec<&str> = Vec::new();
            for companion in companions {
                let lower = companion.name.to_ascii_lowercase();
                let flag = match companion.role {
                    ArtifactRole::Vae => "--vae",
                    ArtifactRole::Decoder => "--taesd",
                    ArtifactRole::Encoder
                        if lower.contains("clip_l") || lower.contains("clip-l") =>
                    {
                        "--clip_l"
                    }
                    ArtifactRole::Encoder
                        if lower.contains("clip_g") || lower.contains("clip-g") =>
                    {
                        "--clip_g"
                    }
                    ArtifactRole::Encoder if lower.contains("t5") => "--t5xxl",
                    ArtifactRole::Encoder => "--llm",
                    _ => return Err(CompanionError::Unsupported),
                };
                if flags.contains(&flag) {
                    return Err(CompanionError::Unsupported);
                }
                flags.push(flag);
                arguments.extend([flag.to_owned(), placeholder(&companion.name)]);
            }
            if !format.standalone()
                && (!flags.contains(&"--vae") || flags.iter().all(|flag| *flag == "--vae"))
            {
                return Err(CompanionError::Required("diffusion_companions_required"));
            }
            Ok(arguments)
        }
        ModelFormat::MoonshineOrt { .. } => {
            crate::runtime_arguments::verify_directory_members(
                companions
                    .iter()
                    .map(|companion| (companion.name.as_str(), companion.role)),
            )
            .map_err(|_| CompanionError::Required("moonshine_companions_required"))?;
            Ok(Vec::new())
        }
        _ if companions.is_empty() => Ok(Vec::new()),
        _ => Err(CompanionError::Unsupported),
    }
}

#[derive(Deserialize)]
struct MoonshineConfig {
    encoder_dim: u64,
    depth: u64,
}

/// Maps a Moonshine streaming configuration to the `moonshine_voice.ModelArch` value the
/// native catalog pins for each published size (tiny 2, small 4, medium 5).
pub fn moonshine_arch(path: &Path) -> Result<u32, FormatError> {
    let file = File::open(path).map_err(|_| FormatError::Unavailable)?;
    let mut bytes = Vec::new();
    file.take(MAX_SPEECH_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FormatError::Unavailable)?;
    if bytes.len() as u64 > MAX_SPEECH_CONFIG_BYTES {
        return Err(FormatError::Invalid("moonshine_config_limit"));
    }
    let config: MoonshineConfig = serde_json::from_slice(&bytes)
        .map_err(|_| FormatError::Invalid("invalid_moonshine_config"))?;
    match (config.encoder_dim, config.depth) {
        (320, 6) => Ok(2),
        (620, 10) => Ok(4),
        (768, 14) => Ok(5),
        _ => Err(FormatError::Invalid("moonshine_architecture_unknown")),
    }
}

#[derive(Clone, Debug)]
pub struct ProfileInputs<'a> {
    pub id: &'a str,
    pub model_file: &'a Path,
    pub artifact: Option<&'a str>,
    pub artifact_set: Option<&'a str>,
    pub package: &'a str,
    pub backend: &'a str,
    pub gpu: bool,
    pub cpu_threads: Option<u64>,
    pub format: &'a ModelFormat,
    pub estimate: &'a Estimate,
    pub companion_arguments: &'a [String],
}

pub fn registration(inputs: &ProfileInputs<'_>) -> Value {
    let engine = inputs.format.engine();
    let threads = inputs
        .cpu_threads
        .filter(|threads| (1..=65536).contains(threads));
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
                if let Some(threads) = threads {
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
        ModelFormat::Diffusion { .. } => {
            let model_flag = if inputs.format.standalone() {
                "--model"
            } else {
                "--diffusion-model"
            };
            let mut args: Vec<String> = [
                "--listen-ip",
                "127.0.0.1",
                "--listen-port",
                "{port}",
                model_flag,
                "{model}",
            ]
            .map(str::to_owned)
            .into();
            args.extend(inputs.companion_arguments.iter().cloned());
            match inputs.backend {
                "vulkan" => args.extend(["--vae-tiling".into(), "--diffusion-fa".into()]),
                "cpu" => {
                    if let Some(threads) = threads {
                        args.extend(["--threads".into(), threads.to_string()]);
                    }
                }
                _ => {}
            }
            (json!(args), json!({}), json!(["images/generations"]))
        }
        ModelFormat::Onnx { .. } => {
            let mut args: Vec<String> = vec!["--model".into(), "{model}".into()];
            args.extend(inputs.companion_arguments.iter().cloned());
            args.extend(["openai", "--ip", "127.0.0.1", "--port", "{port}"].map(str::to_owned));
            (json!(args), json!({}), json!(["audio/speech"]))
        }
        ModelFormat::ParakeetGguf {} => {
            let threads = speech_threads(threads, PARAKEET_MAX_THREADS);
            (
                json!([
                    "--model",
                    "{model}",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "{port}",
                    "--threads",
                    threads,
                    "--concurrency",
                    "1"
                ]),
                json!({}),
                json!(["audio/transcriptions"]),
            )
        }
        ModelFormat::Asr4allGguf {} => {
            let threads = speech_threads(threads, ASR4ALL_MAX_THREADS);
            (
                json!([
                    "--model",
                    "{model}",
                    "--port",
                    "{port}",
                    "--threads",
                    threads,
                    "--futo-commit",
                    ASR4ALL_COMMIT_FRAMES.to_string(),
                    "--futo-right",
                    ASR4ALL_RIGHT_FRAMES.to_string()
                ]),
                json!({}),
                json!(["audio/transcriptions"]),
            )
        }
        ModelFormat::MoonshineOrt { model_arch } => (
            json!([
                "--model-path",
                "{model}",
                "--model-arch",
                model_arch.unwrap_or(MOONSHINE_DEFAULT_ARCH).to_string(),
                "--port",
                "{port}"
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

/// Native recipe defaults from `juiced/BACKENDS.md` (`parakeet_threads`, `asr4all_threads`,
/// `asr4all_commit`, `asr4all_right`), never above the host's usable threads.
fn speech_threads(host: Option<u64>, maximum: u64) -> String {
    host.unwrap_or(SPEECH_DEFAULT_THREADS)
        .clamp(1, SPEECH_DEFAULT_THREADS.min(maximum))
        .to_string()
}

/// A Moonshine set's primary is a generic encoder file, so its default ID is the native
/// catalog name for the detected size.
pub fn native_default_id(format: &ModelFormat) -> Option<String> {
    match format {
        ModelFormat::MoonshineOrt {
            model_arch: Some(arch),
        } => match arch {
            2 => Some("Moonshine-Tiny-Streaming".into()),
            4 => Some("Moonshine-Small-Streaming".into()),
            5 => Some("Moonshine-Medium-Streaming".into()),
            _ => None,
        },
        _ => None,
    }
}

pub fn default_id(name: &str) -> Option<String> {
    let file = name.rsplit(['/', '\\']).next()?;
    let stem = [".gguf", ".bin", ".safetensors", ".onnx", ".ort"]
        .iter()
        .find_map(|suffix| file.strip_suffix(suffix))
        .unwrap_or(file);
    let id: String = stem
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect();
    let id = id.trim().to_owned();
    (!id.is_empty() && !id.contains(',')).then_some(id)
}
