//! llama.cpp memory accounting that mirrors the layout the pinned builds allocate, so
//! reservations and the fit preview agree with `llama-fit-params`.

use crate::profiles::ModelShape;
use serde::Serialize;

const MIB: u64 = 1024 * 1024;
/// llama-server's `--cache-ram` default at b11580 and b12032.
pub const ENGINE_CACHE_RAM_MIB: u64 = 8192;
/// The prompt cache Juiced passes to every llama.cpp launch whose profile leaves
/// `--cache-ram` unset. Juiced's own session cache keeps long-lived conversation state on
/// disk, so the engine's in-RAM copy only needs to cover quick switches between short prompts
/// and stays a small, reserved amount instead of the engine's 8 GiB.
pub const JUICED_CACHE_RAM_MIB: u64 = 1024;
pub const MINIMUM_AUTO_CONTEXT: u64 = 4096;
pub const UNKNOWN_TRAINED_CAP: u64 = 32_768;
const AUTO_CONTEXT_STEP: u64 = 1024;
const DEFAULT_UBATCH: u64 = 512;
const DEFAULT_CHECKPOINTS: u64 = 32;
const DEFAULT_CHECKPOINT_STEP: u64 = 8192;
const KV_PAD: u64 = 256;
const COMPUTE_SLACK: u64 = 16 * MIB;
const DRAFT_WORKSPACE: u64 = 256 * MIB;
const PROJECTOR_WORKSPACE: u64 = 128 * MIB;
const UNKNOWN_VOCABULARY: u64 = 262_144;
const UNKNOWN_EMBEDDING: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
    F32,
    F16,
    Bf16,
    Q8_0,
    Q4_0,
    Q4_1,
    Q5_0,
    Q5_1,
    Iq4Nl,
}

impl CacheType {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "f32" => Self::F32,
            "f16" => Self::F16,
            "bf16" => Self::Bf16,
            "q8_0" => Self::Q8_0,
            "q4_0" => Self::Q4_0,
            "q4_1" => Self::Q4_1,
            "q5_0" => Self::Q5_0,
            "q5_1" => Self::Q5_1,
            "iq4_nl" => Self::Iq4Nl,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::Bf16 => "bf16",
            Self::Q8_0 => "q8_0",
            Self::Q4_0 => "q4_0",
            Self::Q4_1 => "q4_1",
            Self::Q5_0 => "q5_0",
            Self::Q5_1 => "q5_1",
            Self::Iq4Nl => "iq4_nl",
        }
    }
    /// Bytes per block of 32 elements (ggml block layouts).
    fn block_bytes(self) -> u128 {
        match self {
            Self::F32 => 128,
            Self::F16 | Self::Bf16 => 64,
            Self::Q8_0 => 34,
            Self::Q4_0 | Self::Iq4Nl => 18,
            Self::Q4_1 => 20,
            Self::Q5_0 => 22,
            Self::Q5_1 => 24,
        }
    }
    fn bytes(self, elements: u128) -> u128 {
        elements * self.block_bytes() / 32
    }
}

/// The launch arguments that change llama.cpp memory, with engine defaults filled in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub context_tokens: u64,
    pub slots: u64,
    pub gpu: bool,
    pub cache_type_k: CacheType,
    pub cache_type_v: CacheType,
    pub ubatch: u64,
    pub flash_attention: Option<bool>,
    pub swa_full: bool,
    pub kv_unified: bool,
    pub mtp: bool,
    pub cache_ram_mib: Option<i64>,
    pub ctx_checkpoints: u64,
    pub checkpoint_min_step: u64,
}

impl Plan {
    pub fn new(context_tokens: u64, slots: u64, gpu: bool) -> Self {
        Self {
            context_tokens: context_tokens.max(1),
            slots: slots.max(1),
            gpu,
            cache_type_k: CacheType::F16,
            cache_type_v: CacheType::F16,
            ubatch: DEFAULT_UBATCH,
            flash_attention: None,
            swa_full: false,
            kv_unified: false,
            mtp: false,
            cache_ram_mib: Some(JUICED_CACHE_RAM_MIB as i64),
            ctx_checkpoints: DEFAULT_CHECKPOINTS,
            checkpoint_min_step: DEFAULT_CHECKPOINT_STEP,
        }
    }

    /// Reads the memory-relevant flags from a resolved llama-server argument vector. The
    /// context and slot count come from the typed options when the vector leaves them unset.
    pub fn from_arguments(args: &[String], context_tokens: u64, slots: u64, gpu: bool) -> Self {
        let mut plan = Self::new(context_tokens, slots, gpu);
        let mut position = 0;
        while position < args.len() {
            let (flag, inline) = args[position]
                .split_once('=')
                .map_or((args[position].as_str(), None), |(flag, value)| {
                    (flag, Some(value))
                });
            let value = inline.or_else(|| args.get(position + 1).map(String::as_str));
            let consumed = if inline.is_some() { 1 } else { 2 };
            let number = value.and_then(|value| value.parse::<i64>().ok());
            let mut step = consumed;
            match flag {
                "-c" | "--ctx-size" => {
                    if let Some(value) = number.filter(|value| *value > 0) {
                        plan.context_tokens = value as u64;
                    }
                }
                "-np" | "--parallel" => {
                    if let Some(value) = number.filter(|value| *value > 0) {
                        plan.slots = value as u64;
                    }
                }
                "-ngl" | "--gpu-layers" | "--n-gpu-layers" => {
                    if number == Some(0) {
                        plan.gpu = false;
                    }
                }
                "-ctk" | "--cache-type-k" => {
                    if let Some(kind) = value.and_then(CacheType::parse) {
                        plan.cache_type_k = kind;
                    }
                }
                "-ctv" | "--cache-type-v" => {
                    if let Some(kind) = value.and_then(CacheType::parse) {
                        plan.cache_type_v = kind;
                    }
                }
                "-ub" | "--ubatch-size" => {
                    if let Some(value) = number.filter(|value| *value > 0) {
                        plan.ubatch = value as u64;
                    }
                }
                "-fa" | "--flash-attn" => {
                    plan.flash_attention = match value {
                        Some("on" | "1" | "true" | "enabled") => Some(true),
                        Some("off" | "0" | "false" | "disabled") => Some(false),
                        _ => None,
                    }
                }
                "-cram" | "--cache-ram" => plan.cache_ram_mib = number,
                "-ctxcp" | "--ctx-checkpoints" | "--swa-checkpoints" => {
                    if let Some(value) = number.filter(|value| *value >= 0) {
                        plan.ctx_checkpoints = value as u64;
                    }
                }
                "-cms" | "--checkpoint-min-step" => {
                    if let Some(value) = number.filter(|value| *value > 0) {
                        plan.checkpoint_min_step = value as u64;
                    }
                }
                "--spec-type" => plan.mtp = value == Some("draft-mtp"),
                "--swa-full" => {
                    plan.swa_full = true;
                    step = 1;
                }
                "-kvu" | "--kv-unified" => {
                    plan.kv_unified = true;
                    step = 1;
                }
                "-no-kvu" | "--no-kv-unified" => {
                    plan.kv_unified = false;
                    step = 1;
                }
                _ => step = 1,
            }
            position += step;
        }
        plan
    }

    fn streams(&self) -> u64 {
        if self.kv_unified { 1 } else { self.slots }
    }
    fn context_per_stream(&self) -> u64 {
        pad(self.context_tokens / self.streams()).max(KV_PAD)
    }
}

fn pad(value: u64) -> u64 {
    value.div_ceil(KV_PAD).saturating_mul(KV_PAD)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Breakdown {
    pub weights_bytes: u64,
    pub context_bytes: u64,
    pub compute_bytes: u64,
    pub projector_bytes: u64,
    pub draft_bytes: u64,
    pub other_bytes: u64,
}

impl Breakdown {
    pub fn total(&self) -> u64 {
        [
            self.weights_bytes,
            self.context_bytes,
            self.compute_bytes,
            self.projector_bytes,
            self.draft_bytes,
            self.other_bytes,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layer {
    kv_heads: u64,
    key: u64,
    value: u64,
}

struct Layout {
    full: Vec<Layer>,
    sliding: Vec<Layer>,
    window: u64,
    recurrent_layers: u64,
    recurrent_elements: u64,
    draft: Vec<Layer>,
}

/// llama.cpp's built-in SWA layer period for architectures whose GGUF stores only the window.
fn swa_period(architecture: Option<&str>) -> Option<u64> {
    match architecture? {
        "gemma2" | "gpt-oss" => Some(2),
        "gemma3" => Some(6),
        "gemma3n" => Some(5),
        "cohere2" | "exaone4" => Some(4),
        _ => None,
    }
}

fn layout(shape: &ModelShape, architecture: Option<&str>) -> Option<Layout> {
    let blocks = shape.block_count.filter(|blocks| *blocks > 0)?;
    let nextn = shape.nextn_predict_layers.unwrap_or(0).min(blocks);
    let main = blocks - nextn;
    let kv_layers = main.saturating_sub(shape.shared_kv_layers.unwrap_or(0).min(main));
    let width = shape
        .embedding_length
        .zip(shape.head_count)
        .and_then(|(embedding, heads)| embedding.checked_div(heads));
    let key = shape.key_length.or(width)?;
    let value = shape.value_length.or(width)?;
    let scalar_heads = shape.head_count_kv.or(shape.head_count);
    let heads_at = |layer: u64| -> Option<u64> {
        shape
            .head_count_kv_layers
            .as_ref()
            .and_then(|layers| layers.get(layer as usize).copied())
            .or(scalar_heads)
    };
    let mamba = architecture.is_some_and(|name| name.starts_with("mamba"));
    let parallel_hybrid = architecture == Some("falcon-h1");
    let interval = shape
        .full_attention_interval
        .filter(|interval| *interval > 1);
    let period = shape
        .sliding_window_period
        .filter(|period| *period > 0)
        .or_else(|| swa_period(architecture));
    let window = shape.sliding_window.filter(|window| *window > 0);
    let state = recurrent_elements(shape);
    let mut layout = Layout {
        full: Vec::new(),
        sliding: Vec::new(),
        window: window.unwrap_or(0),
        recurrent_layers: 0,
        recurrent_elements: state,
        draft: Vec::new(),
    };
    for layer in 0..main {
        let heads = heads_at(layer)?;
        let attention = !mamba
            && match interval {
                Some(interval) => (layer + 1) % interval == 0,
                None => heads > 0,
            };
        let recurrent = mamba
            || parallel_hybrid
            || (state > 0 && interval.is_some() && !attention)
            || (state > 0 && interval.is_none() && heads == 0);
        if recurrent {
            layout.recurrent_layers += 1;
        }
        if !attention || layer >= kv_layers {
            continue;
        }
        let sliding = window.is_some()
            && match &shape.sliding_window_pattern {
                Some(pattern) => pattern.get(layer as usize).copied().unwrap_or(false),
                None => period.is_some_and(|period| layer % period < period - 1),
            };
        if sliding {
            layout.sliding.push(Layer {
                kv_heads: heads,
                key: shape.key_length_swa.unwrap_or(key),
                value: shape.value_length_swa.unwrap_or(value),
            });
        } else {
            layout.full.push(Layer {
                kv_heads: heads,
                key,
                value,
            });
        }
    }
    for layer in main..blocks {
        let heads = heads_at(layer)
            .filter(|heads| *heads > 0)
            .or(scalar_heads)?;
        layout.draft.push(Layer {
            kv_heads: heads,
            key,
            value,
        });
    }
    Some(layout)
}

/// Per-sequence f32 elements of recurrent state in one layer (llama.cpp `n_embd_r` +
/// `n_embd_s`).
fn recurrent_elements(shape: &ModelShape) -> u64 {
    if let Some(cache) = shape.shortconv_cache.filter(|cache| *cache > 1) {
        return shape
            .embedding_length
            .unwrap_or(0)
            .saturating_mul(cache - 1);
    }
    let (Some(conv), Some(inner), Some(state)) = (
        shape.ssm_conv_kernel,
        shape.ssm_inner_size,
        shape.ssm_state_size,
    ) else {
        return 0;
    };
    let groups = shape.ssm_group_count.unwrap_or(0);
    let convolution = conv
        .saturating_sub(1)
        .saturating_mul(inner.saturating_add(2u64.saturating_mul(groups).saturating_mul(state)));
    convolution.saturating_add(state.saturating_mul(inner))
}

fn layer_bytes(layers: &[Layer], cells: u64, plan: &Plan) -> u128 {
    layers
        .iter()
        .map(|layer| {
            let k = u128::from(layer.kv_heads) * u128::from(layer.key) * u128::from(cells);
            let v = u128::from(layer.kv_heads) * u128::from(layer.value) * u128::from(cells);
            plan.cache_type_k.bytes(k) + plan.cache_type_v.bytes(v)
        })
        .sum()
}

fn clamp(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// The breakdown llama.cpp would allocate for this GGUF and plan, plus what llama-server adds
/// beyond `llama-fit-params`: projector, MTP draft context, prompt cache and context
/// checkpoints. `None` when the metadata lacks the attention shape.
pub fn llama(
    shape: &ModelShape,
    architecture: Option<&str>,
    model_bytes: u64,
    projector_bytes: u64,
    plan: &Plan,
) -> Option<Breakdown> {
    let layout = layout(shape, architecture)?;
    let streams = plan.streams();
    let full_cells = streams.saturating_mul(plan.context_per_stream());
    let sliding_cells_per_stream = if plan.swa_full {
        plan.context_per_stream()
    } else if plan.kv_unified {
        plan.context_per_stream().min(pad(layout
            .window
            .saturating_mul(plan.slots)
            .saturating_add(plan.ubatch)))
    } else {
        plan.context_per_stream()
            .min(pad(layout.window.saturating_add(plan.ubatch)))
    };
    let sliding_cells = streams.saturating_mul(sliding_cells_per_stream);
    let recurrent_per_sequence =
        u128::from(layout.recurrent_layers) * u128::from(layout.recurrent_elements) * 4;
    let context = layer_bytes(&layout.full, full_cells, plan)
        + layer_bytes(&layout.sliding, sliding_cells, plan)
        + recurrent_per_sequence * u128::from(plan.slots);
    let context_bytes = clamp(context);

    let ubatch = u128::from(plan.ubatch);
    let attention = u128::from(plan.context_per_stream());
    let vocabulary = u128::from(shape.vocab_size.unwrap_or(UNKNOWN_VOCABULARY));
    let embedding = u128::from(shape.embedding_length.unwrap_or(UNKNOWN_EMBEDDING));
    let mask = attention * ubatch * 2;
    let mut compute = ubatch * 4 * (vocabulary + 6 * embedding) + mask + u128::from(COMPUTE_SLACK);
    if plan.flash_attention == Some(false) {
        compute += attention * ubatch * u128::from(shape.head_count.unwrap_or(32)) * 8;
    }
    if plan.gpu {
        compute += mask + ubatch * embedding * 16 + u128::from(COMPUTE_SLACK);
    }

    let tied = if plan.gpu {
        shape.tied_embedding_bytes.unwrap_or(0)
    } else {
        0
    };
    let projector = if projector_bytes > 0 {
        projector_bytes.saturating_add((projector_bytes / 4).max(PROJECTOR_WORKSPACE))
    } else {
        0
    };
    let draft = if plan.mtp && !layout.draft.is_empty() {
        clamp(layer_bytes(&layout.draft, full_cells, plan)).saturating_add(DRAFT_WORKSPACE)
    } else {
        0
    };

    let cache_limit = match plan.cache_ram_mib {
        None => ENGINE_CACHE_RAM_MIB.saturating_mul(MIB),
        Some(0) => 0,
        Some(limit) if limit < 0 => u64::MAX,
        Some(limit) => (limit as u64).saturating_mul(MIB),
    };
    // A cached prompt state holds at most the launch context, so the cache stops growing
    // there even when its MiB limit is larger.
    let prompt_cache = cache_limit.min(context_bytes);
    let checkpoint_state = recurrent_per_sequence
        + if plan.swa_full {
            0
        } else {
            layer_bytes(&layout.sliding, sliding_cells_per_stream, plan)
        };
    let checkpoints = if checkpoint_state > 0 {
        let count = plan
            .ctx_checkpoints
            .min(plan.context_per_stream() / plan.checkpoint_min_step.max(1) + 1);
        clamp(checkpoint_state * u128::from(count) * u128::from(plan.slots))
    } else {
        0
    };

    Some(Breakdown {
        weights_bytes: model_bytes.saturating_add(tied),
        context_bytes,
        compute_bytes: clamp(compute),
        projector_bytes: projector,
        draft_bytes: draft,
        other_bytes: prompt_cache.saturating_add(checkpoints),
    })
}

pub fn has_mtp_head(shape: &ModelShape) -> bool {
    shape.nextn_predict_layers.is_some_and(|layers| layers > 0)
}

/// The largest context (a multiple of 1024, at least 4096 and at most the trained context, or
/// 32768 when it is unknown) whose breakdown fits `share`; the minimum when nothing fits.
pub fn auto_context(
    trained: Option<u64>,
    share: u64,
    total_at: impl Fn(u64) -> Option<u64>,
) -> u64 {
    let cap = trained
        .filter(|trained| *trained > 0)
        .unwrap_or(UNKNOWN_TRAINED_CAP)
        .min(i32::MAX as u64);
    let floor = MINIMUM_AUTO_CONTEXT.min(cap);
    let fits = |context: u64| total_at(context).is_some_and(|total| total <= share);
    if fits(cap) {
        return cap;
    }
    let (mut low, mut high) = (floor / AUTO_CONTEXT_STEP, cap / AUTO_CONTEXT_STEP);
    if low == 0 || !fits(low * AUTO_CONTEXT_STEP) {
        return floor;
    }
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(middle * AUTO_CONTEXT_STEP) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low * AUTO_CONTEXT_STEP
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FitDevice {
    pub device: String,
    pub model_bytes: u64,
    pub context_bytes: u64,
    pub compute_bytes: u64,
}

/// Parses `llama-fit-params --fit-print on` stdout: one `<device> <model> <context> <compute>`
/// line per device in MiB.
pub fn parse_fit_print(output: &str) -> Option<Vec<FitDevice>> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let [name @ .., model, context, compute] = fields.as_slice() else {
            return None;
        };
        if name.is_empty() || name.len() > 4 {
            return None;
        }
        let mib = |value: &str| value.parse::<u64>().ok()?.checked_mul(MIB);
        devices.push(FitDevice {
            device: name.join(" "),
            model_bytes: mib(model)?,
            context_bytes: mib(context)?,
            compute_bytes: mib(compute)?,
        });
        if devices.len() > 64 {
            return None;
        }
    }
    (!devices.is_empty()).then_some(devices)
}

/// Replaces the engine-measured parts of an estimate with llama-fit-params' figures. Weights
/// never drop below the file size: CPU builds report repacked weights outside the printed
/// devices, and the MTP head's weights load only with a draft context.
pub fn with_fit(estimate: &Breakdown, devices: &[FitDevice]) -> Breakdown {
    let sum =
        |field: fn(&FitDevice) -> u64| devices.iter().map(field).fold(0u64, u64::saturating_add);
    Breakdown {
        weights_bytes: sum(|device| device.model_bytes).max(estimate.weights_bytes),
        context_bytes: sum(|device| device.context_bytes),
        compute_bytes: sum(|device| device.compute_bytes),
        ..*estimate
    }
}

/// llama-server flags `llama-fit-params` accepts and that change its memory layout.
const FIT_VALUES: &[&str] = &[
    "-c",
    "--ctx-size",
    "-np",
    "--parallel",
    "-ngl",
    "--gpu-layers",
    "--n-gpu-layers",
    "-ctk",
    "--cache-type-k",
    "-ctv",
    "--cache-type-v",
    "-fa",
    "--flash-attn",
    "-b",
    "--batch-size",
    "-ub",
    "--ubatch-size",
    "-sm",
    "--split-mode",
    "-ts",
    "--tensor-split",
    "-mg",
    "--main-gpu",
    "-dev",
    "--device",
    "-ot",
    "--override-tensor",
    "-ncmoe",
    "--n-cpu-moe",
];
const FIT_SWITCHES: &[&str] = &[
    "--swa-full",
    "-kvu",
    "--kv-unified",
    "-no-kvu",
    "--no-kv-unified",
    "-cmoe",
    "--cpu-moe",
    "-nkvo",
    "--no-kv-offload",
];

/// The subset of a resolved launch vector that `llama-fit-params` takes, with the model
/// placeholder replaced by the model path.
pub fn fit_arguments(args: &[String], model: &str) -> Vec<String> {
    let mut selected = vec!["-m".to_owned(), model.to_owned()];
    let mut position = 0;
    while position < args.len() {
        let (flag, inline) = args[position]
            .split_once('=')
            .map_or((args[position].as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        if FIT_SWITCHES.contains(&flag) {
            selected.push(flag.to_owned());
            position += 1;
        } else if FIT_VALUES.contains(&flag) {
            let value = inline.or_else(|| args.get(position + 1).map(String::as_str));
            if let Some(value) = value {
                selected.extend([flag.to_owned(), value.to_owned()]);
            }
            position += if inline.is_some() { 1 } else { 2 };
        } else {
            position += 1;
        }
    }
    selected.extend(["--fit-print".to_owned(), "on".to_owned()]);
    selected
}

/// Sampler and template defaults native Juiced applies per GGUF architecture
/// (`architecture_defaults.json` `llamacpp_args`).
pub fn architecture_arguments(architecture: Option<&str>) -> Vec<String> {
    static DEFAULTS: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    let defaults = DEFAULTS.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../../src/cpp/resources/architecture_defaults.json"
        ))
        .unwrap_or_default()
    });
    let Some(text) = architecture
        .and_then(|name| defaults.get(name))
        .and_then(|entry| entry.get("llamacpp_args"))
        .and_then(serde_json::Value::as_str)
    else {
        return Vec::new();
    };
    split_arguments(text)
}

fn split_arguments(text: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for character in text.chars() {
        match (quote, character) {
            (Some(open), close) if open == close => quote = None,
            (Some(_), other) => current.push(other),
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (None, space) if space.is_whitespace() => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, other) => {
                current.push(other);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// Flag spellings llama-server treats as the same option, so a default never duplicates an
/// explicit argument written with another alias.
const ALIASES: &[&[&str]] = &[
    &["--temp", "--temperature"],
    &["--top-k"],
    &["--top-p"],
    &["--min-p"],
    &["--repeat-penalty"],
    &["--presence-penalty"],
    &["--frequency-penalty"],
    &["--chat-template-kwargs"],
    &["--spec-type"],
    &["-cram", "--cache-ram"],
    &["--metrics"],
];

fn aliases(flag: &str) -> &'static [&'static str] {
    ALIASES
        .iter()
        .find(|group| group.contains(&flag))
        .copied()
        .unwrap_or(&[])
}

pub fn has_flag(args: &[String], flag: &str) -> bool {
    let group = aliases(flag);
    args.iter().any(|argument| {
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(name, _)| name);
        name == flag || group.contains(&name)
    })
}

/// What Juiced adds to a llama.cpp launch that leaves these choices unset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlamaDefaults {
    pub mtp: bool,
    pub architecture: Vec<String>,
}

impl LlamaDefaults {
    pub fn for_model(shape: &ModelShape, architecture: Option<&str>, slots: u64) -> Self {
        Self {
            mtp: has_mtp_head(shape) && slots == 1,
            architecture: architecture_arguments(architecture),
        }
    }

    /// Appends each default whose option the arguments do not already set.
    pub fn apply(&self, args: &mut Vec<String>) {
        if !has_flag(args, "--metrics") {
            args.push("--metrics".into());
        }
        if self.mtp && !has_flag(args, "--spec-type") {
            args.extend(["--spec-type".into(), "draft-mtp".into()]);
        }
        let mut position = 0;
        while position < self.architecture.len() {
            let flag = &self.architecture[position];
            let value = self
                .architecture
                .get(position + 1)
                .filter(|value| !value.starts_with("--"));
            if !has_flag(args, flag) {
                args.push(flag.clone());
                if let Some(value) = value {
                    args.push(value.clone());
                }
            }
            position += if value.is_some() { 2 } else { 1 };
        }
    }
}

#[cfg(test)]
mod tests;
