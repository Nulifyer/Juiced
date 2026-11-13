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
/// Per-request activations the generation engines allocate beyond their weights: ACE-Step's
/// LM KV cache, DiT and VAE buffers (the 3.05 GiB turbo set held 9.0 GiB on CUDA with
/// `--keep-loaded`), ThinkSound's VAE im2col buffers and TRELLIS' sparse-structure decode.
pub const AUDIO_GENERATION_WORKSPACE_BYTES: u64 = 6 * 1024 * 1024 * 1024;
pub const SOUND_EFFECT_WORKSPACE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
pub const MESH_WORKSPACE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MOSS_WORKSPACE_BYTES: u64 = 1024 * 1024 * 1024;
/// The primary GGUF of each generation engine; the other files of its set are companions.
pub const ACESTEP_PRIMARY: &str = "acestep-dit";
pub const THINKSOUND_PRIMARY: &str = "thinksound-mmdit";
pub const TRELLIS_PRIMARY: &str = "trellis2-ss-flow";
const GENERATION_COMPONENTS: &[&str] = &[
    "acestep-lm",
    "acestep-text-enc",
    "acestep-vae",
    "thinksound-t5",
    "thinksound-metaclip-text",
    "thinksound-vae",
    "thinksound-t5-tok",
    "thinksound-clip-tok",
    "trellis2-ss-dec",
    "trellis2-shape-flow",
    "trellis2-shape-dec",
    "trellis2-slat-flow",
    "trellis2-tex-dec",
    "dinov3-vitl16",
    "birefnet-swinl",
    "moss_tts_delay",
    "moss_tts_local",
    "moss_soundeffect",
];
pub const MOSS_SOUND_EFFECT: &str = "moss_soundeffect";
const MOSS_FAMILIES: &[&str] = &["moss_tts_delay", "moss_tts_local", MOSS_SOUND_EFFECT];
const TRELLIS_RESOLUTION: &str = "512";
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
        shape: Box<ModelShape>,
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
    /// An exported encoder classifier directory (`model.onnx`, `tokenizer.json`,
    /// `config.json`) for `ort-server`, registered by its `model.onnx`.
    OnnxClassifier {
        architecture: Option<String>,
        labels: u32,
    },
    /// An original Transformers checkpoint directory, registered by its `config.json`.
    Transformers {
        checkpoint: Box<crate::vllm::Checkpoint>,
    },
    #[serde(rename = "acestep-gguf")]
    AceStepGguf {},
    #[serde(rename = "thinksound-gguf")]
    ThinkSoundGguf {},
    TrellisGguf {},
    /// An OpenMOSS backbone; `family` comes from its `.extras.gguf` sidecar.
    #[serde(rename = "openmoss-gguf")]
    MossGguf {
        family: String,
    },
    /// Split DiT, VAE and text-encoder safetensors that TheNoise serves.
    #[serde(rename = "thenoise-diffusion")]
    TheNoiseDiffusion {
        architecture: Option<String>,
    },
}

impl ModelFormat {
    /// Engines that can serve this format; the first is the automatic choice.
    pub fn engines(&self) -> Vec<Engine> {
        match self {
            Self::Gguf { .. } => vec![Engine::LlamaCpp, Engine::Ds4],
            other => vec![other.engine()],
        }
    }
    pub fn engine(&self) -> Engine {
        match self {
            Self::Gguf { .. } => Engine::LlamaCpp,
            Self::WhisperGgml { .. } => Engine::WhisperCpp,
            Self::Diffusion { .. } => Engine::StableDiffusionCpp,
            Self::Onnx { .. } => Engine::Kokoros,
            Self::ParakeetGguf { .. } => Engine::Parakeet,
            Self::Asr4allGguf { .. } => Engine::Asr4all,
            Self::MoonshineOrt { .. } => Engine::Moonshine,
            Self::Transformers { .. } => Engine::Vllm,
            Self::OnnxClassifier { .. } => Engine::OnnxRuntime,
            Self::AceStepGguf {} => Engine::AceStep,
            Self::ThinkSoundGguf {} => Engine::ThinkSound,
            Self::TrellisGguf {} => Engine::Trellis,
            Self::MossGguf { .. } => Engine::OpenMoss,
            Self::TheNoiseDiffusion { .. } => Engine::TheNoise,
        }
    }
    /// Engines that read a whole model directory (or a sidecar beside the model) by file name.
    pub fn directory(&self) -> bool {
        matches!(
            self,
            Self::MoonshineOrt { .. }
                | Self::AceStepGguf {}
                | Self::ThinkSoundGguf {}
                | Self::TrellisGguf {}
                | Self::MossGguf { .. }
        )
    }
    pub fn standalone(&self) -> bool {
        match self {
            Self::Diffusion {
                includes_vae,
                includes_text_encoder,
                ..
            } => *includes_vae && *includes_text_encoder,
            Self::Onnx { .. }
            | Self::MoonshineOrt { .. }
            | Self::AceStepGguf {}
            | Self::ThinkSoundGguf {}
            | Self::TrellisGguf {}
            | Self::MossGguf { .. }
            | Self::TheNoiseDiffusion { .. } => false,
            Self::Gguf { .. }
            | Self::WhisperGgml { .. }
            | Self::ParakeetGguf { .. }
            | Self::Asr4allGguf { .. }
            | Self::Transformers { .. }
            | Self::OnnxClassifier { .. } => true,
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

/// `ort-server` serves a directory with the graph, the tokenizer and the config.
fn onnx_classifier(path: &Path) -> Option<ModelFormat> {
    let directory = path.parent()?;
    if path.file_name()? != "model.onnx" || !directory.join("tokenizer.json").is_file() {
        return None;
    }
    let file = File::open(directory.join("config.json")).ok()?;
    if file.metadata().ok()?.len() > MAX_SPEECH_CONFIG_BYTES * 16 {
        return None;
    }
    let config: Value = serde_json::from_reader(BufReader::new(file)).ok()?;
    Some(ModelFormat::OnnxClassifier {
        architecture: config["architectures"][0].as_str().map(str::to_owned),
        labels: config["id2label"]
            .as_object()
            .map_or(0, |labels| labels.len() as u32),
    })
}

pub fn detect(path: &Path) -> Result<(ModelFormat, u64), FormatError> {
    if let Some(format) = onnx_classifier(path) {
        let bytes = std::fs::metadata(path)
            .map_err(|_| FormatError::Unavailable)?
            .len();
        return Ok((format, bytes));
    }
    if path.file_name().is_some_and(|name| name == "config.json") {
        let checkpoint = crate::vllm::checkpoint(path).ok_or(FormatError::Unsupported)?;
        let bytes = checkpoint.weights_bytes;
        return Ok((
            ModelFormat::Transformers {
                checkpoint: Box::new(checkpoint),
            },
            bytes,
        ));
    }
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
            Some(ACESTEP_PRIMARY) => return Ok((ModelFormat::AceStepGguf {}, bytes)),
            Some(THINKSOUND_PRIMARY) => return Ok((ModelFormat::ThinkSoundGguf {}, bytes)),
            Some(TRELLIS_PRIMARY) => return Ok((ModelFormat::TrellisGguf {}, bytes)),
            Some(component) if GENERATION_COMPONENTS.contains(&component) => {
                return Err(FormatError::Invalid(COMPONENT_NOT_PRIMARY));
            }
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
        return Ok((
            ModelFormat::Gguf {
                declared,
                shape: Box::new(shape),
            },
            bytes,
        ));
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

pub const COMPONENT_NOT_PRIMARY: &str = "generation_component_not_primary";

/// Detects a checkpoint artifact set (a Transformers `config.json` or an ONNX classifier's
/// `model.onnx`) by linking its members under their names, as the launch does; returns the
/// primary member's size.
pub fn detect_checkpoint_set(
    primary: &str,
    path: &Path,
    companions: &[(&str, &Path)],
) -> Result<(ModelFormat, u64), FormatError> {
    let directory = tempfile::Builder::new()
        .prefix("juiced-checkpoint-")
        .tempdir()
        .map_err(|_| FormatError::Unavailable)?;
    for (name, target) in std::iter::once((primary, path)).chain(companions.iter().copied()) {
        if !crate::runtime_arguments::valid_member_name(name) {
            return Err(FormatError::Invalid("artifact_set_binding_mismatch"));
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, directory.path().join(name))
            .map_err(|_| FormatError::Unavailable)?;
        #[cfg(not(unix))]
        std::fs::hard_link(target, directory.path().join(name))
            .map_err(|_| FormatError::Unavailable)?;
    }
    let (format, _) = detect(&directory.path().join(primary))?;
    match format {
        ModelFormat::Transformers { .. } | ModelFormat::OnnxClassifier { .. } => {
            let bytes = std::fs::metadata(path)
                .map_err(|_| FormatError::Unavailable)?
                .len();
            Ok((format, bytes))
        }
        _ => Err(FormatError::Unsupported),
    }
}

/// Primary member names that anchor a checkpoint directory set.
pub fn checkpoint_primary(name: &str) -> bool {
    matches!(name, "config.json" | "model.onnx")
}

/// The OpenMOSS family a sidecar declares, or `None` for any other GGUF.
pub fn moss_family(path: &Path) -> Result<Option<String>, FormatError> {
    let mut file = File::open(path).map_err(|_| FormatError::Unavailable)?;
    let mut magic = [0; 4];
    if read_prefix(&mut file, &mut magic)? < 4 || &magic != b"GGUF" {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| FormatError::Unavailable)?;
    let (declared, _) = crate::profiles::read_gguf_shape(&mut file)
        .map_err(|error| FormatError::Invalid(error.code))?;
    Ok(declared
        .architecture
        .filter(|architecture| MOSS_FAMILIES.contains(&architecture.as_str())))
}

/// Native ThinkSound and TRELLIS checkpoints name a whole repository, so the weights found
/// may be any of its networks: the primary is the directory's DiT or sparse-structure flow,
/// and every GGUF beside it is charged as part of the model.
pub fn generation_directory(found: crate::native_import::Weights) -> crate::native_import::Weights {
    let Some(directory) = found.path.parent() else {
        return found;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return found;
    };
    let mut files: Vec<(std::path::PathBuf, u64)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "gguf")
        })
        .filter_map(|entry| Some((entry.path(), std::fs::metadata(entry.path()).ok()?.len())))
        .filter(|(_, bytes)| *bytes > 0)
        .take(64)
        .collect();
    files.sort();
    let primary = files.iter().find(|(path, _)| {
        File::open(path).ok().is_some_and(|mut file| {
            crate::profiles::read_gguf_shape(&mut file).is_ok_and(|(declared, _)| {
                matches!(
                    declared.architecture.as_deref(),
                    Some(ACESTEP_PRIMARY | THINKSOUND_PRIMARY | TRELLIS_PRIMARY)
                )
            })
        })
    });
    match primary {
        Some((path, _)) => crate::native_import::Weights {
            path: path.clone(),
            bytes: files.iter().map(|(_, bytes)| bytes).sum(),
            files: files.clone(),
            selection: "generation-directory",
            snapshot: found.snapshot,
        },
        None => found,
    }
}

/// The sidecar name moss-tts-server derives from its `--model` path.
pub fn moss_sidecar(model: &str) -> String {
    format!(
        "{}.extras.gguf",
        model.strip_suffix(".gguf").unwrap_or(model)
    )
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<super::memory::Breakdown>,
}

/// GGUF chat models are charged the layout llama.cpp allocates for a single-slot GPU launch
/// with Juiced's defaults (see [`super::memory::llama`]). When the shape is incomplete, KV is
/// charged as a quarter of the weight file instead. Image and speech workers charge their
/// weights, companions and a fixed per-engine workspace.
pub fn estimate(format: &ModelFormat, file_bytes: u64, context: Option<u64>) -> Estimate {
    let plan = context.map(|context| super::memory::Plan::new(context, 1, true));
    estimate_with(format, file_bytes, 0, plan.as_ref())
}

/// `model_bytes` excludes `projector_bytes`; the plan carries the launch context and flags.
pub fn estimate_with(
    format: &ModelFormat,
    model_bytes: u64,
    projector_bytes: u64,
    plan: Option<&super::memory::Plan>,
) -> Estimate {
    let file_bytes = model_bytes.saturating_add(projector_bytes);
    if let ModelFormat::Transformers { checkpoint } = format {
        let context = plan.map_or_else(
            || crate::vllm::default_context(checkpoint),
            |plan| plan.context_tokens,
        );
        let memory = crate::vllm::memory(checkpoint, context);
        return Estimate {
            file_bytes,
            context_tokens: Some(context),
            kv_bytes: memory.kv_bytes,
            workspace_bytes: crate::vllm::RUNTIME_OVERHEAD_BYTES,
            resident_bytes: memory.resident_bytes,
            basis: "vllm-kv-budget",
            formula: "safetensors weights + --kv-cache-memory-bytes (16-bit KV for the context) + 2 GiB runtime",
            breakdown: None,
        };
    }
    match format {
        ModelFormat::Diffusion { .. }
        | ModelFormat::Onnx { .. }
        | ModelFormat::OnnxClassifier { .. }
        | ModelFormat::ParakeetGguf {}
        | ModelFormat::Asr4allGguf {}
        | ModelFormat::MoonshineOrt { .. }
        | ModelFormat::AceStepGguf {}
        | ModelFormat::ThinkSoundGguf {}
        | ModelFormat::TrellisGguf {}
        | ModelFormat::MossGguf { .. }
        | ModelFormat::TheNoiseDiffusion { .. } => {
            let workspace = match format {
                ModelFormat::Diffusion { .. } | ModelFormat::TheNoiseDiffusion { .. } => {
                    DIFFUSION_WORKSPACE_BYTES
                }
                ModelFormat::AceStepGguf {} => AUDIO_GENERATION_WORKSPACE_BYTES,
                ModelFormat::ThinkSoundGguf {} => SOUND_EFFECT_WORKSPACE_BYTES,
                ModelFormat::TrellisGguf {} => MESH_WORKSPACE_BYTES,
                ModelFormat::MossGguf { .. } => MOSS_WORKSPACE_BYTES,
                _ => SPEECH_WORKSPACE_BYTES,
            };
            return Estimate {
                file_bytes,
                context_tokens: None,
                kv_bytes: 0,
                workspace_bytes: workspace,
                resident_bytes: file_bytes.saturating_add(workspace),
                basis: "weights-plus-workspace",
                formula: "file_bytes (including companions) + fixed engine workspace",
                breakdown: None,
            };
        }
        _ => {}
    }
    if let (ModelFormat::Gguf { declared, shape }, Some(plan)) = (format, plan)
        && let Some(breakdown) = super::memory::llama(
            shape,
            declared.architecture.as_deref(),
            model_bytes,
            projector_bytes,
            plan,
        )
    {
        let resident_bytes = breakdown.total();
        return Estimate {
            file_bytes,
            context_tokens: Some(plan.context_tokens),
            kv_bytes: breakdown.context_bytes,
            workspace_bytes: resident_bytes
                .saturating_sub(file_bytes)
                .saturating_sub(breakdown.context_bytes),
            resident_bytes,
            basis: "gguf-layout",
            formula: "weights + attention KV (hybrid, SWA and per-layer heads) + recurrent state + compute + projector + MTP draft + prompt cache and checkpoints",
            breakdown: Some(breakdown),
        };
    }
    let context = plan.map(|plan| plan.context_tokens);
    let (kv_bytes, basis) = match (format, context) {
        (ModelFormat::Gguf { .. }, Some(_)) => (file_bytes / 4, "file-fraction"),
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
        formula: "file_bytes + file_bytes/4 + 256 MiB",
        breakdown: None,
    }
}

/// The reservation earlier releases computed for a GGUF registration: every block charged full
/// f16 KV plus a flat 256 MiB. A stored reservation equal to it was generated automatically.
pub fn legacy_resident_bytes(format: &ModelFormat, file_bytes: u64, context: u64) -> u64 {
    let kv = match format {
        ModelFormat::Gguf { shape, .. } => kv_bytes(shape, context).unwrap_or(file_bytes / 4),
        _ => 0,
    };
    file_bytes
        .saturating_add(kv)
        .saturating_add(WORKSPACE_ALLOWANCE_BYTES)
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
        ModelFormat::Transformers { checkpoint } => Some(crate::vllm::default_context(checkpoint)),
        ModelFormat::WhisperGgml { .. }
        | ModelFormat::Diffusion { .. }
        | ModelFormat::Onnx { .. }
        | ModelFormat::OnnxClassifier { .. }
        | ModelFormat::ParakeetGguf {}
        | ModelFormat::Asr4allGguf {}
        | ModelFormat::MoonshineOrt { .. }
        | ModelFormat::AceStepGguf {}
        | ModelFormat::ThinkSoundGguf {}
        | ModelFormat::TrellisGguf {}
        | ModelFormat::MossGguf { .. }
        | ModelFormat::TheNoiseDiffusion { .. } => None,
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
        ModelFormat::AceStepGguf {}
        | ModelFormat::ThinkSoundGguf {}
        | ModelFormat::TrellisGguf {}
        | ModelFormat::MossGguf { .. } => {
            crate::runtime_arguments::verify_named_members(
                companions
                    .iter()
                    .map(|companion| (companion.name.as_str(), companion.role)),
            )
            .map_err(|_| CompanionError::Unsupported)?;
            match format {
                ModelFormat::AceStepGguf {} | ModelFormat::ThinkSoundGguf {}
                    if companions.is_empty() =>
                {
                    Err(CompanionError::Required("generation_companions_required"))
                }
                ModelFormat::TrellisGguf {} if companions.len() < 5 => {
                    Err(CompanionError::Required("generation_companions_required"))
                }
                _ => Ok(Vec::new()),
            }
        }
        ModelFormat::TheNoiseDiffusion { .. } => {
            let mut vae = None;
            let mut encoder = None;
            for companion in companions {
                let slot = match companion.role {
                    ArtifactRole::Vae => &mut vae,
                    ArtifactRole::Encoder => &mut encoder,
                    _ => return Err(CompanionError::Unsupported),
                };
                if slot.replace(&companion.name).is_some() {
                    return Err(CompanionError::Unsupported);
                }
            }
            match (vae, encoder) {
                (Some(vae), Some(encoder)) => Ok(vec![
                    "--vae".into(),
                    placeholder(vae),
                    "--text-encoder".into(),
                    placeholder(encoder),
                ]),
                _ => Err(CompanionError::Required("thenoise_companions_required")),
            }
        }
        ModelFormat::Transformers { .. } | ModelFormat::OnnxClassifier { .. } => {
            crate::runtime_arguments::verify_checkpoint_members(
                companions
                    .iter()
                    .map(|companion| (companion.name.as_str(), companion.role)),
            )
            .map_err(|_| CompanionError::Unsupported)?;
            Ok(Vec::new())
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
    pub llama: Option<&'a super::memory::LlamaDefaults>,
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
            let mut args: Vec<String> = [
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--model",
                "{model}",
                "--slots",
                "--jinja",
            ]
            .map(str::to_owned)
            .into();
            if let Some(defaults) = inputs.llama {
                defaults.apply(&mut args);
            }
            (
                json!(args),
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
        ModelFormat::Transformers { checkpoint } => {
            let context = inputs
                .estimate
                .context_tokens
                .unwrap_or_else(|| crate::vllm::default_context(checkpoint));
            let mut args: Vec<String> = ["-I", "-B", "-m", crate::vllm::MODULE, "serve", "{model}"]
                .map(str::to_owned)
                .into();
            args.extend([
                "--port".into(),
                "{port}".into(),
                "--served-model-name".into(),
                inputs.id.to_owned(),
                "--max-model-len".into(),
                context.to_string(),
                "--kv-cache-memory-bytes".into(),
                inputs.estimate.kv_bytes.to_string(),
                "--max-num-seqs".into(),
                crate::vllm::DEFAULT_MAX_SEQUENCES.to_string(),
            ]);
            if inputs.backend == "rocm" {
                args.push("--enforce-eager".into());
            }
            args.extend(crate::vllm::family_arguments(
                &checkpoint.architecture,
                None,
            ));
            (
                json!(args),
                json!({}),
                json!(["chat/completions", "completions", "responses"]),
            )
        }
        ModelFormat::OnnxClassifier { .. } => (
            json!(["--model-path", "{model}", "--port", "{port}"]),
            json!({}),
            json!(["classify"]),
        ),
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
        ModelFormat::AceStepGguf {} => (
            json!([
                "--models",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--keep-loaded"
            ]),
            json!({}),
            json!(["audio/generations"]),
        ),
        ModelFormat::ThinkSoundGguf {} => (
            json!([
                "--dir",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}"
            ]),
            json!({}),
            json!(["audio/generations"]),
        ),
        ModelFormat::TrellisGguf {} => {
            let mut args: Vec<String> = [
                "--models",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--res",
                TRELLIS_RESOLUTION,
            ]
            .map(str::to_owned)
            .into();
            if inputs.gpu {
                args.push("--require-gpu".into());
            }
            (json!(args), json!({}), json!(["3d/generations"]))
        }
        ModelFormat::MossGguf { family } => (
            json!([
                "--model",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--no-webui"
            ]),
            json!({}),
            if family == MOSS_SOUND_EFFECT {
                json!(["audio/generations"])
            } else {
                json!(["audio/speech"])
            },
        ),
        ModelFormat::TheNoiseDiffusion { .. } => {
            let mut args: Vec<String> = vec!["serve".into(), "--dit".into(), "{model}".into()];
            args.extend(inputs.companion_arguments.iter().cloned());
            args.extend(["--host", "127.0.0.1", "--port", "{port}"].map(str::to_owned));
            (json!(args), json!({}), json!(["images/generations"]))
        }
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
        "max_inflight": if matches!(inputs.format, ModelFormat::Transformers { .. }) {
            crate::vllm::DEFAULT_MAX_SEQUENCES
        } else {
            1
        },
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

/// Rewrites a GGUF registration for `ds4-server`, which serves only its curated
/// DeepSeek V4 GGUFs. Native streamed experts from disk (`--ssd-streaming`) because full
/// residency of the published 81 GB model never fits gfx1151's arena.
pub fn ds4_registration(registration: &mut Value, context: Option<u64>, file_bytes: u64) {
    let mut args: Vec<String> = ["-m", "{model}", "--host", "127.0.0.1", "--port", "{port}"]
        .map(str::to_owned)
        .into();
    if let Some(context) = context {
        args.extend(["-c".into(), context.to_string()]);
    }
    args.push("--ssd-streaming".into());
    let profile = &mut registration["execution_profile"];
    profile["adapter"] = json!(crate::config::Provider::Ds4);
    profile["implementation_id"] = json!(crate::config::Provider::Ds4.implementation_id());
    profile["args"] = json!(args);
    profile["options"] = json!({});
    // Streaming keeps routed experts on disk; native charged the published model 16 GB.
    let streamed = (file_bytes / 5).max(16 * 1024 * 1024 * 1024);
    if registration["resident_bytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > streamed)
    {
        registration["resident_bytes"] = json!(streamed);
    }
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
