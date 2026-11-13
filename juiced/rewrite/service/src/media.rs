use crate::{ApiError, auth::RequestContext, extract::ApiBytes, http::AppState};
use axum::{
    Extension,
    body::Bytes,
    extract::{Multipart, State, multipart::MultipartError},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::borrow::Cow;

pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_SPEECH_INPUT_BYTES: usize = 16 * 1024;
pub const MAX_IMAGE_RESPONSE_BYTES: usize = 48 * 1024 * 1024;
pub const MAX_SPEECH_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
pub const MIN_IMAGE_SIDE: u32 = 64;
pub const MAX_IMAGE_SIDE: u32 = 2048;
const MAX_STEPS: u64 = 150;
const MAX_CFG: f64 = 50.0;
const MAX_FLOW_SHIFT: f64 = 100.0;
const MAX_SEED: i64 = u32::MAX as i64;
const EXTRA_ARGS: &str = "sd_cpp_extra_args";
pub const MAX_EDIT_IMAGES: usize = 8;
pub const MAX_EDIT_OUTPUTS: u64 = 10;
pub const MAX_TXT2IMG_BATCH: u64 = 4;

/// OpenAI voice names that Kokoros maps onto its own voices.
pub const OPENAI_VOICE_ALIASES: &[(&str, &str)] = &[
    ("alloy", "af_alloy"),
    ("echo", "am_echo"),
    ("nova", "af_nova"),
    ("onyx", "am_onyx"),
    ("shimmer", "af_sky"),
    ("fable", "af_bella"),
    ("coral", "af_nicole"),
    ("sage", "af_sarah"),
    ("marin", "af_river"),
    ("ash", "am_adam"),
    ("ballad", "am_michael"),
    ("verse", "am_eric"),
    ("cedar", "am_liam"),
];

fn invalid(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}
fn unsupported(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "unsupported_parameter", message)
}

fn object(body: &[u8], code: &'static str) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(invalid(code, "Send one JSON object.")),
    }
}

fn model(fields: &Map<String, Value>, code: &'static str) -> Result<Option<String>, ApiError> {
    match fields.get("model") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(model))
            if !model.trim().is_empty()
                && model.len() <= 256
                && !model.chars().any(char::is_control) =>
        {
            Ok(Some(model.clone()))
        }
        _ => Err(invalid(code, "model must be a registered model ID.")),
    }
}

fn text(
    fields: &Map<String, Value>,
    key: &str,
    limit: usize,
    code: &'static str,
) -> Result<Option<String>, ApiError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() > limit => Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "media_field_limit",
            "A text field exceeds its byte limit.",
        )),
        Some(Value::String(value)) if value.contains('\0') => {
            Err(invalid(code, "Text fields cannot contain NUL bytes."))
        }
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(invalid(code, "Text fields must be strings.")),
    }
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-.".contains(&byte))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sampling {
    pub negative_prompt: Option<String>,
    pub steps: Option<u64>,
    pub cfg_scale: Option<f64>,
    pub seed: Option<i64>,
    pub sampler: Option<String>,
    pub scheduler: Option<String>,
}

impl Sampling {
    fn parse(
        fields: &Map<String, Value>,
        keys: [&str; 6],
        code: &'static str,
    ) -> Result<Self, ApiError> {
        let [negative, steps, cfg, seed, sampler, scheduler] = keys;
        let integer = |key: &str, range: std::ops::RangeInclusive<i64>| match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_i64()
                .filter(|value| range.contains(value))
                .map(Some)
                .ok_or_else(|| invalid(code, "A numeric sampling field is outside its range.")),
        };
        let cfg_scale = match fields.get(cfg) {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .filter(|value| value.is_finite() && (0.0..=MAX_CFG).contains(value))
                    .ok_or_else(|| invalid(code, "cfg_scale must be a number from 0 to 50."))?,
            ),
        };
        let name = |key: &str| -> Result<Option<String>, ApiError> {
            match text(fields, key, 32, code)? {
                Some(value) if !token(&value) => Err(invalid(
                    code,
                    "Sampler and scheduler names must be short identifiers.",
                )),
                value => Ok(value),
            }
        };
        let negative_prompt = text(fields, negative, MAX_PROMPT_BYTES, code)?;
        if negative_prompt
            .as_deref()
            .is_some_and(|value| value.contains(EXTRA_ARGS))
        {
            return Err(invalid(code, "Prompts cannot embed engine argument tags."));
        }
        Ok(Self {
            negative_prompt,
            steps: integer(steps, 1..=MAX_STEPS as i64)?.map(|value| value as u64),
            cfg_scale,
            seed: integer(seed, -1..=MAX_SEED)?,
            sampler: name(sampler)?,
            scheduler: name(scheduler)?,
        })
    }

    fn extra_arguments(&self) -> Value {
        let mut sample = Map::new();
        if let Some(steps) = self.steps {
            sample.insert("sample_steps".into(), json!(steps));
        }
        if let Some(cfg) = self.cfg_scale {
            sample.insert("guidance".into(), json!({"txt_cfg": cfg}));
        }
        if let Some(method) = &self.sampler {
            sample.insert("sample_method".into(), json!(method));
        }
        if let Some(scheduler) = &self.scheduler {
            sample.insert("scheduler".into(), json!(scheduler));
        }
        let mut extra = Map::new();
        if !sample.is_empty() {
            extra.insert("sample_params".into(), Value::Object(sample));
        }
        if let Some(seed) = self.seed {
            extra.insert("seed".into(), json!(seed));
        }
        if let Some(negative) = &self.negative_prompt {
            extra.insert("negative_prompt".into(), json!(negative));
        }
        Value::Object(extra)
    }
}

fn reject_unknown(
    fields: &Map<String, Value>,
    allowed: &[&str],
    message: &'static str,
) -> Result<(), ApiError> {
    if fields.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(unsupported(message))
    }
}

fn prompt(fields: &Map<String, Value>, code: &'static str) -> Result<String, ApiError> {
    let prompt = text(fields, "prompt", MAX_PROMPT_BYTES, code)?
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| invalid(code, "A nonempty prompt is required."))?;
    if prompt.contains(EXTRA_ARGS) {
        return Err(invalid(code, "Prompts cannot embed engine argument tags."));
    }
    Ok(prompt)
}

fn side(value: u64) -> Option<u32> {
    u32::try_from(value).ok().filter(|value| {
        (MIN_IMAGE_SIDE..=MAX_IMAGE_SIDE).contains(value) && value.is_multiple_of(8)
    })
}

fn dimension(value: u64, code: &'static str) -> Result<u32, ApiError> {
    side(value).ok_or_else(|| {
        invalid(
            code,
            "Image sides must be multiples of 8 from 64 to 2048 pixels.",
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

impl ImageFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "png" => Some(Self::Png),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }
    fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            Self::Webp => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        }
    }
    fn detect(bytes: &[u8]) -> Option<Self> {
        [Self::Png, Self::Jpeg, Self::Webp]
            .into_iter()
            .find(|format| format.matches(bytes))
    }
    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}
fn le_bytes(bytes: &[u8], at: usize, count: usize) -> Option<u32> {
    let slice = bytes.get(at..at + count)?;
    Some(
        slice
            .iter()
            .rev()
            .fold(0u32, |word, byte| (word << 8) | u32::from(*byte)),
    )
}

/// Reads the pixel size from PNG, JPEG or WebP headers without decoding the image.
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let (width, height) = match ImageFormat::detect(bytes)? {
        ImageFormat::Png => {
            if bytes.get(12..16)? != b"IHDR" {
                return None;
            }
            (
                u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
                u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?),
            )
        }
        ImageFormat::Jpeg => {
            let mut at = 2;
            loop {
                while *bytes.get(at)? == 0xff && *bytes.get(at + 1)? == 0xff {
                    at += 1;
                }
                if *bytes.get(at)? != 0xff {
                    return None;
                }
                let marker = *bytes.get(at + 1)?;
                if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                    break (be16(bytes, at + 7)?, be16(bytes, at + 5)?);
                }
                if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
                    at += 2;
                    continue;
                }
                at += 2 + be16(bytes, at + 2)? as usize;
            }
        }
        ImageFormat::Webp => match bytes.get(12..16)? {
            b"VP8X" => (le_bytes(bytes, 24, 3)? + 1, le_bytes(bytes, 27, 3)? + 1),
            b"VP8L" if *bytes.get(20)? == 0x2f => {
                let bits = le_bytes(bytes, 21, 4)?;
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8 " if bytes.get(23..26)? == [0x9d, 0x01, 0x2a] => (
                le_bytes(bytes, 26, 2)? & 0x3fff,
                le_bytes(bytes, 28, 2)? & 0x3fff,
            ),
            _ => return None,
        },
    };
    (width > 0 && height > 0).then_some((width, height))
}

/// Scales a source size to the service's side limit, keeping the aspect ratio, with sides
/// rounded down to the multiple of 8 that sd.cpp latents need.
pub fn fitted_size(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(MAX_IMAGE_SIDE) / f64::from(width.max(height))).min(1.0);
    let fit = |value: u32| {
        ((((f64::from(value) * scale) as u32) / 8) * 8).clamp(MIN_IMAGE_SIDE, MAX_IMAGE_SIDE)
    };
    (fit(width), fit(height))
}

fn tagged_prompt(prompt: &str, sampling: &Sampling) -> String {
    let extra = sampling.extra_arguments();
    if extra.as_object().is_some_and(Map::is_empty) {
        prompt.to_owned()
    } else {
        format!("{prompt} <{EXTRA_ARGS}>{extra}</{EXTRA_ARGS}>")
    }
}

#[derive(Debug, PartialEq)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    /// Absent when the client set no size, so the worker applies its configured default.
    pub size: Option<(u32, u32)>,
    pub format: ImageFormat,
    pub sampling: Sampling,
}

const IMAGE_FIELDS: &[&str] = &[
    "model",
    "prompt",
    "n",
    "size",
    "response_format",
    "output_format",
    "user",
    "negative_prompt",
    "steps",
    "cfg_scale",
    "seed",
    "sample_method",
    "scheduler",
];

impl ImageRequest {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        const CODE: &str = "invalid_image_request";
        let fields = object(body, CODE)?;
        reject_unknown(
            &fields,
            IMAGE_FIELDS,
            "Use only supported image generation fields; editing, masks, styles and quality tiers are unavailable.",
        )?;
        let model = model(&fields, CODE)?
            .ok_or_else(|| invalid(CODE, "A registered image model ID is required."))?;
        if fields
            .get("n")
            .is_some_and(|n| !n.is_null() && n.as_u64() != Some(1))
        {
            return Err(unsupported("Only n=1 is supported."));
        }
        match fields.get("response_format") {
            None | Some(Value::Null) => {}
            Some(Value::String(format)) if format == "b64_json" => {}
            _ => return Err(unsupported("Only response_format b64_json is supported.")),
        }
        let format = match fields.get("output_format") {
            None | Some(Value::Null) => ImageFormat::Png,
            Some(Value::String(format)) => ImageFormat::parse(format)
                .ok_or_else(|| unsupported("output_format must be png, jpeg or webp."))?,
            _ => return Err(unsupported("output_format must be png, jpeg or webp.")),
        };
        if fields.get("user").is_some_and(|user| !user.is_string()) {
            return Err(invalid(CODE, "user must be a string."));
        }
        let size = match fields.get("size") {
            None | Some(Value::Null) => None,
            Some(Value::String(size)) if size == "auto" => None,
            Some(Value::String(size)) => {
                let (width, height) = size
                    .split_once('x')
                    .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                    .ok_or_else(|| invalid(CODE, "size must look like 512x512."))?;
                Some((dimension(width, CODE)?, dimension(height, CODE)?))
            }
            _ => return Err(invalid(CODE, "size must look like 512x512.")),
        };
        Ok(Self {
            model,
            prompt: prompt(&fields, CODE)?,
            size,
            format,
            sampling: Sampling::parse(
                &fields,
                [
                    "negative_prompt",
                    "steps",
                    "cfg_scale",
                    "seed",
                    "sample_method",
                    "scheduler",
                ],
                CODE,
            )?,
        })
    }

    /// sd-server reads per-request sampling only from a tagged JSON suffix in the prompt.
    pub fn upstream(&self) -> Vec<u8> {
        let mut body = json!({
            "prompt": tagged_prompt(&self.prompt, &self.sampling),
            "n": 1,
            "output_format": self.format.name(),
        });
        if let Some((width, height)) = self.size {
            body["size"] = json!(format!("{width}x{height}"));
        }
        serde_json::to_vec(&body).expect("image request serializes")
    }
}

#[derive(Debug, PartialEq)]
pub struct Txt2ImgRequest {
    pub model: Option<String>,
    pub batch: u64,
    pub body: Map<String, Value>,
}

const SDAPI_FIELDS: &[&str] = &[
    "model",
    "prompt",
    "negative_prompt",
    "width",
    "height",
    "steps",
    "cfg_scale",
    "seed",
    "batch_size",
    "n_iter",
    "sampler_name",
    "scheduler",
    "clip_skip",
    "override_settings",
    "send_images",
    "enable_hr",
    "restore_faces",
    "alwayson_scripts",
];

/// A1111 clients such as SillyTavern always send these. They only matter for high-resolution
/// passes or server-side saving, neither of which runs, so they are dropped. SillyTavern also
/// forwards its own `url` and `auth` settings in the body; they are dropped unread.
const SDAPI_INERT: &[&str] = &[
    "override_settings_restore_afterwards",
    "save_images",
    "do_not_save_grid",
    "do_not_save_samples",
    "hr_upscaler",
    "hr_scale",
    "hr_additional_modules",
    "hr_second_pass_steps",
    "denoising_strength",
    "url",
    "auth",
];

impl Txt2ImgRequest {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        const CODE: &str = "invalid_txt2img_request";
        let mut fields = object(body, CODE)?;
        if fields.keys().any(|key| {
            !SDAPI_FIELDS.contains(&key.as_str()) && !SDAPI_INERT.contains(&key.as_str())
        }) {
            return Err(unsupported(
                "Use only the supported txt2img subset; LoRA files, image inputs and other extensions are unavailable.",
            ));
        }
        fields.retain(|key, _| !SDAPI_INERT.contains(&key.as_str()));
        for key in ["sampler_name", "scheduler"] {
            if fields
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|name| matches!(name, "" | "N/A"))
            {
                fields.remove(key);
            }
        }
        let requested = model(&fields, CODE)?;
        if fields
            .get("n_iter")
            .is_some_and(|value| !value.is_null() && value.as_u64() != Some(1))
        {
            return Err(unsupported(
                "Only one iteration per txt2img request is supported.",
            ));
        }
        let batch = match fields.get("batch_size") {
            None | Some(Value::Null) => 1,
            Some(value) => value
                .as_u64()
                .filter(|count| (1..=MAX_TXT2IMG_BATCH).contains(count))
                .ok_or_else(|| unsupported("batch_size must be from 1 to 4."))?,
        };
        for (key, expected) in [
            ("enable_hr", false),
            ("restore_faces", false),
            ("send_images", true),
        ] {
            match fields.get(key) {
                None | Some(Value::Null) => {}
                Some(Value::Bool(value)) if *value == expected => {}
                _ => {
                    return Err(unsupported(
                        "Only returned images without high-resolution passes or face restoration are supported.",
                    ));
                }
            }
        }
        match fields.get("alwayson_scripts") {
            None | Some(Value::Null) => {}
            Some(Value::Object(scripts)) if scripts.is_empty() => {}
            _ => return Err(unsupported("Scripts and extensions are unavailable.")),
        }
        let mut checkpoint = None;
        let mut clip_layers = None;
        match fields.get("override_settings") {
            None | Some(Value::Null) => {}
            Some(Value::Object(overrides)) => {
                for (key, value) in overrides {
                    match (key.as_str(), value) {
                        (_, Value::Null) => {}
                        ("sd_model_checkpoint", Value::String(name)) => {
                            checkpoint =
                                model(&Map::from_iter([("model".to_owned(), json!(name))]), CODE)?;
                        }
                        ("CLIP_stop_at_last_layers", value) => {
                            clip_layers = Some(
                                value
                                    .as_i64()
                                    .filter(|value| (1..=12).contains(value))
                                    .ok_or_else(|| {
                                        invalid(
                                            CODE,
                                            "CLIP_stop_at_last_layers must be from 1 to 12.",
                                        )
                                    })?,
                            );
                        }
                        ("forge_additional_modules", Value::Array(modules))
                            if modules.is_empty() => {}
                        _ => {
                            return Err(unsupported(
                                "override_settings may only set sd_model_checkpoint or CLIP_stop_at_last_layers.",
                            ));
                        }
                    }
                }
            }
            _ => return Err(invalid(CODE, "override_settings must be an object.")),
        }
        let model = match (requested, checkpoint) {
            (Some(left), Some(right)) if left != right => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "conflicting_model_selection",
                    "model and override_settings.sd_model_checkpoint name different models.",
                ));
            }
            (left, right) => left.or(right),
        };
        let side = |key: &str| match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .ok_or_else(|| invalid(CODE, "width and height must be integers."))
                .and_then(|value| dimension(value, CODE))
                .map(Some),
        };
        let (width, height) = (side("width")?, side("height")?);
        let prompt = prompt(&fields, CODE)?;
        let sampling = Sampling::parse(
            &fields,
            [
                "negative_prompt",
                "steps",
                "cfg_scale",
                "seed",
                "sampler_name",
                "scheduler",
            ],
            CODE,
        )?;
        let clip_skip = match fields.get("clip_skip") {
            None | Some(Value::Null) => clip_layers,
            Some(value) => Some(
                value
                    .as_i64()
                    .filter(|value| (-1..=12).contains(value))
                    .ok_or_else(|| invalid(CODE, "clip_skip must be from -1 to 12."))?,
            ),
        };
        let mut body = Map::new();
        body.insert("prompt".into(), json!(prompt));
        if let Some(width) = width {
            body.insert("width".into(), json!(width));
        }
        if let Some(height) = height {
            body.insert("height".into(), json!(height));
        }
        body.insert("batch_size".into(), json!(batch));
        if let Some(negative) = sampling.negative_prompt {
            body.insert("negative_prompt".into(), json!(negative));
        }
        if let Some(steps) = sampling.steps {
            body.insert("steps".into(), json!(steps));
        }
        if let Some(cfg) = sampling.cfg_scale {
            body.insert("cfg_scale".into(), json!(cfg));
        }
        if let Some(seed) = sampling.seed {
            body.insert("seed".into(), json!(seed));
        }
        if let Some(sampler) = sampling.sampler {
            body.insert("sampler_name".into(), json!(sampler));
        }
        if let Some(scheduler) = sampling.scheduler {
            body.insert("scheduler".into(), json!(scheduler));
        }
        if let Some(clip_skip) = clip_skip {
            body.insert("clip_skip".into(), json!(clip_skip));
        }
        Ok(Self { model, batch, body })
    }

    pub fn upstream(&self) -> Vec<u8> {
        serde_json::to_vec(&self.body).expect("txt2img request serializes")
    }
}

#[derive(Debug, PartialEq)]
pub struct ImageUpload {
    pub bytes: Bytes,
    pub format: ImageFormat,
}

/// One `/images/edits` or `/images/variations` form, forwarded to sd-server's edit endpoint.
#[derive(Debug, PartialEq)]
pub struct EditRequest {
    pub model: String,
    pub prompt: String,
    pub images: Vec<ImageUpload>,
    pub mask: Option<ImageUpload>,
    pub n: u64,
    pub size: Option<(u32, u32)>,
    pub format: ImageFormat,
    pub sampling: Sampling,
}

const EDIT_TEXT_FIELDS: &[&str] = &[
    "model",
    "prompt",
    "size",
    "n",
    "response_format",
    "output_format",
    "output_compression",
    "user",
    "background",
    "quality",
    "input_fidelity",
    "stream",
    "negative_prompt",
    "steps",
    "cfg_scale",
    "seed",
    "sample_method",
    "scheduler",
];

fn edit_multipart_error(error: MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "media_field_limit",
            "The image form exceeds its 8 MiB upload limit.",
        )
    } else {
        invalid(
            "invalid_image_request",
            "The image form is malformed multipart/form-data.",
        )
    }
}

impl EditRequest {
    pub async fn parse(mut multipart: Multipart, variation: bool) -> Result<Self, ApiError> {
        const CODE: &str = "invalid_image_request";
        let mut form = Map::new();
        let mut images = Vec::new();
        let (mut single, mut listed) = (false, false);
        let mut mask = None;
        while let Some(field) = multipart.next_field().await.map_err(edit_multipart_error)? {
            let name = field.name().unwrap_or_default().to_owned();
            if matches!(name.as_str(), "image" | "image[]" | "mask") {
                let bytes = field.bytes().await.map_err(edit_multipart_error)?;
                let format = ImageFormat::detect(&bytes).ok_or_else(|| {
                    invalid(CODE, "Image uploads must be PNG, JPEG or WebP files.")
                })?;
                let upload = ImageUpload { bytes, format };
                if name == "mask" {
                    if mask.replace(upload).is_some() {
                        return Err(invalid(CODE, "Send at most one mask."));
                    }
                    continue;
                }
                if name == "image" {
                    single = true;
                } else {
                    listed = true;
                }
                if images.len() == MAX_EDIT_IMAGES {
                    return Err(invalid(CODE, "Send at most 8 reference images."));
                }
                images.push(upload);
                continue;
            }
            // v0.9.2 skipped unrecognized form fields, so clients sending extras keep working.
            if !EDIT_TEXT_FIELDS.contains(&name.as_str()) {
                field.bytes().await.map_err(edit_multipart_error)?;
                continue;
            }
            if field.file_name().is_some() {
                return Err(invalid(CODE, "Text form fields cannot be file uploads."));
            }
            let bytes = field.bytes().await.map_err(edit_multipart_error)?;
            let value = String::from_utf8(bytes.to_vec())
                .map_err(|_| invalid(CODE, "Text form fields must be valid UTF-8."))?;
            if form.insert(name, Value::String(value)).is_some() {
                return Err(invalid(CODE, "Form fields must not be repeated."));
            }
        }
        if single && listed {
            return Err(invalid(CODE, "Use either 'image' or 'image[]', not both."));
        }
        if images.is_empty() {
            return Err(invalid(CODE, "Missing 'image' field in request."));
        }
        if variation && (images.len() > 1 || mask.is_some()) {
            return Err(unsupported(
                "Variations take exactly one image and no mask.",
            ));
        }
        for (key, kind) in [
            ("steps", "an integer"),
            ("seed", "an integer"),
            ("n", "an integer"),
            ("output_compression", "an integer"),
            ("cfg_scale", "a number"),
        ] {
            let Some(text) = form.get(key).and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            let number = if kind == "a number" {
                text.parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .and_then(serde_json::Number::from_f64)
            } else {
                text.parse::<i64>().ok().map(serde_json::Number::from)
            };
            let number = number.ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    CODE,
                    format!("Invalid value for '{key}': must be {kind}"),
                )
            })?;
            form.insert(key.to_owned(), Value::Number(number));
        }
        let field = |key: &str| form.get(key).and_then(Value::as_str);
        let model = field("model")
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| invalid(CODE, "Missing 'model' field in request."))?
            .to_owned();
        let model = self::model(&Map::from_iter([("model".to_owned(), json!(model))]), CODE)?
            .expect("present model");
        let prompt = if variation {
            "variation".to_owned()
        } else {
            if field("prompt").is_none_or(|prompt| prompt.trim().is_empty()) {
                return Err(invalid(CODE, "Missing 'prompt' field in request."));
            }
            prompt(&form, CODE)?
        };
        let n = match form.get("n") {
            None => 1,
            Some(value) => value
                .as_u64()
                .filter(|n| (1..=MAX_EDIT_OUTPUTS).contains(n))
                .ok_or_else(|| invalid(CODE, "Invalid value for 'n': must be between 1 and 10"))?,
        };
        if form
            .get("output_compression")
            .is_some_and(|value| value.as_u64().is_none_or(|value| value > 100))
        {
            return Err(invalid(CODE, "output_compression must be from 0 to 100."));
        }
        match field("response_format") {
            None | Some("b64_json") => {}
            _ => return Err(unsupported("Only response_format b64_json is supported.")),
        }
        match field("stream") {
            None | Some("false") => {}
            _ => return Err(unsupported("Only complete image responses are supported.")),
        }
        let format = match field("output_format") {
            None | Some("png") => ImageFormat::Png,
            Some("jpeg" | "jpg") => ImageFormat::Jpeg,
            _ => return Err(unsupported("output_format must be png or jpeg for edits.")),
        };
        let size = match field("size") {
            None | Some("auto") => {
                image_dimensions(&images[0].bytes).map(|(width, height)| fitted_size(width, height))
            }
            Some(size) => {
                let (width, height) = size
                    .split_once('x')
                    .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                    .ok_or_else(|| invalid(CODE, "size must look like 512x512."))?;
                Some((dimension(width, CODE)?, dimension(height, CODE)?))
            }
        };
        let sampling = Sampling::parse(
            &form,
            [
                "negative_prompt",
                "steps",
                "cfg_scale",
                "seed",
                "sample_method",
                "scheduler",
            ],
            CODE,
        )?;
        Ok(Self {
            model,
            prompt,
            images,
            mask,
            n,
            size,
            format,
            sampling,
        })
    }

    pub fn upstream(&self) -> reqwest::multipart::Form {
        let part = |index: usize, upload: &ImageUpload| {
            let name = format!("image-{index}.{}", upload.format.name());
            reqwest::multipart::Part::stream_with_length(
                reqwest::Body::from(upload.bytes.clone()),
                upload.bytes.len() as u64,
            )
            .file_name(name)
            .mime_str(upload.format.mime())
            .expect("static image media type")
        };
        let mut form = reqwest::multipart::Form::new()
            .text("prompt", tagged_prompt(&self.prompt, &self.sampling))
            .text("n", self.n.to_string())
            .text("output_format", self.format.name());
        if let Some((width, height)) = self.size {
            form = form.text("size", format!("{width}x{height}"));
        }
        for (index, image) in self.images.iter().enumerate() {
            form = form.part("image[]", part(index, image));
        }
        if let Some(mask) = &self.mask {
            form = form.part("mask", part(self.images.len(), mask));
        }
        form
    }
}

/// sd-server launch flags that serve as per-model image defaults; a request field overrides
/// its default. The first flag is the one written.
pub const IMAGE_DEFAULTS: &[(&str, &[&str])] = &[
    ("steps", &["--steps"]),
    ("cfg_scale", &["--cfg-scale"]),
    ("width", &["-W", "--width"]),
    ("height", &["-H", "--height"]),
    ("sampling_method", &["--sampling-method"]),
    ("scheduler", &["--scheduler"]),
    ("flow_shift", &["--flow-shift"]),
    ("seed", &["-s", "--seed"]),
];

/// The sd-server argument for one image default, or `None` when the key is unknown or the value
/// is outside its range.
pub fn image_default_argument(key: &str, value: &Value) -> Option<String> {
    match key {
        "steps" => value
            .as_u64()
            .filter(|steps| (1..=MAX_STEPS).contains(steps))
            .map(|steps| steps.to_string()),
        "width" | "height" => value.as_u64().and_then(side).map(|side| side.to_string()),
        "seed" => value
            .as_i64()
            .filter(|seed| (-1..=MAX_SEED).contains(seed))
            .map(|seed| seed.to_string()),
        "cfg_scale" | "flow_shift" => value
            .as_f64()
            .filter(|number| {
                let maximum = if key == "cfg_scale" {
                    MAX_CFG
                } else {
                    MAX_FLOW_SHIFT
                };
                number.is_finite() && (0.0..=maximum).contains(number)
            })
            .map(|number| number.to_string()),
        "sampling_method" | "scheduler" => {
            value.as_str().filter(|name| token(name)).map(str::to_owned)
        }
        _ => None,
    }
}

fn image_flag(argument: &str) -> Option<(usize, bool)> {
    let (flag, inline) = argument
        .split_once('=')
        .map_or((argument, false), |(flag, _)| (flag, true));
    IMAGE_DEFAULTS
        .iter()
        .position(|(_, flags)| flags.contains(&flag))
        .map(|index| (index, inline))
}

/// All image defaults read from launch arguments, `null` where unset.
pub fn image_defaults_view(args: &[String]) -> Value {
    let mut view: Map<String, Value> = IMAGE_DEFAULTS
        .iter()
        .map(|(key, _)| ((*key).to_owned(), Value::Null))
        .collect();
    let mut position = 0;
    while position < args.len() {
        let Some((index, inline)) = image_flag(&args[position]) else {
            position += 1;
            continue;
        };
        let key = IMAGE_DEFAULTS[index].0;
        let text = if inline {
            args[position].split_once('=').map(|(_, value)| value)
        } else {
            args.get(position + 1).map(String::as_str)
        };
        if let Some(text) = text
            && view[key].is_null()
        {
            let parsed = match key {
                "steps" | "width" | "height" => text.parse::<u64>().ok().map(Value::from),
                "seed" => text.parse::<i64>().ok().map(Value::from),
                "cfg_scale" | "flow_shift" => text
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number),
                _ => None,
            };
            view.insert(key.to_owned(), parsed.unwrap_or_else(|| json!(text)));
        }
        position += if inline { 1 } else { 2 };
    }
    Value::Object(view)
}

/// Applies a merge patch to launch arguments: a value replaces that default, `null` removes it
/// and unnamed defaults keep their arguments.
pub fn apply_image_defaults(
    args: &[String],
    patch: &Map<String, Value>,
) -> Result<Vec<String>, ApiError> {
    const CODE: &str = "invalid_image_defaults";
    let mut replacements = Vec::with_capacity(patch.len());
    for (key, value) in patch {
        let index = IMAGE_DEFAULTS
            .iter()
            .position(|(name, _)| name == key)
            .ok_or_else(|| {
                invalid(
                    CODE,
                    "Use only steps, cfg_scale, width, height, sampling_method, scheduler, flow_shift and seed.",
                )
            })?;
        let argument = match value {
            Value::Null => None,
            value => Some(image_default_argument(key, value).ok_or_else(|| {
                invalid(
                    CODE,
                    "Image defaults need steps 1 to 150, cfg_scale 0 to 50, width and height as multiples of 8 from 64 to 2048, flow_shift 0 to 100, seed -1 to 4294967295, and short sampler and scheduler names.",
                )
            })?),
        };
        replacements.push((index, argument));
    }
    replacements.sort_by_key(|(index, _)| *index);
    let mut output = Vec::with_capacity(args.len() + 2 * replacements.len());
    let mut position = 0;
    while position < args.len() {
        match image_flag(&args[position]) {
            Some((index, inline)) if replacements.iter().any(|(named, _)| *named == index) => {
                position += if inline { 1 } else { 2 };
            }
            _ => {
                output.push(args[position].clone());
                position += 1;
            }
        }
    }
    for (index, argument) in replacements {
        if let Some(argument) = argument {
            output.push(IMAGE_DEFAULTS[index].1[0].to_owned());
            output.push(argument);
        }
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Mp3,
    Opus,
    Pcm,
}

impl AudioFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "wav" => Some(Self::Wav),
            "mp3" => Some(Self::Mp3),
            "opus" => Some(Self::Opus),
            "pcm" => Some(Self::Pcm),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Pcm => "pcm",
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Pcm => "audio/pcm",
        }
    }
    fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Wav => bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
            Self::Mp3 => {
                bytes.starts_with(b"ID3")
                    || (bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0)
            }
            Self::Opus => bytes.starts_with(b"OggS"),
            Self::Pcm => !bytes.is_empty() && bytes.len().is_multiple_of(2),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct SpeechRequest {
    pub model: String,
    pub input: String,
    pub voice: String,
    pub format: AudioFormat,
    pub speed: f64,
}

const SPEECH_FIELDS: &[&str] = &[
    "model",
    "input",
    "voice",
    "response_format",
    "speed",
    "stream_format",
];

pub fn valid_voice_selection(voice: &str) -> bool {
    !voice.is_empty()
        && voice.len() <= 256
        && voice.split('+').all(|part| {
            let name = part.split_once('.').map_or(part, |(name, weight)| {
                if weight.bytes().all(|byte| byte.is_ascii_digit()) && !weight.is_empty() {
                    name
                } else {
                    ""
                }
            });
            crate::engines::models::valid_voice(name)
        })
}

impl SpeechRequest {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        const CODE: &str = "invalid_speech_request";
        let fields = object(body, CODE)?;
        reject_unknown(
            &fields,
            SPEECH_FIELDS,
            "Use only supported speech fields; instructions and streaming are unavailable.",
        )?;
        let model = model(&fields, CODE)?
            .ok_or_else(|| invalid(CODE, "A registered speech model ID is required."))?;
        let input = text(&fields, "input", MAX_SPEECH_INPUT_BYTES, CODE)?
            .filter(|input| !input.trim().is_empty())
            .ok_or_else(|| invalid(CODE, "A nonempty input is required."))?;
        let voice = text(&fields, "voice", 256, CODE)?
            .ok_or_else(|| invalid(CODE, "A voice is required."))?;
        if !valid_voice_selection(&voice) {
            return Err(invalid(
                CODE,
                "voice must name a voice, or a blend like af_sarah.4+af_nicole.6.",
            ));
        }
        let format = match fields.get("response_format") {
            None | Some(Value::Null) => AudioFormat::Mp3,
            Some(Value::String(format)) => AudioFormat::parse(format)
                .ok_or_else(|| unsupported("response_format must be wav, mp3, opus or pcm."))?,
            _ => {
                return Err(unsupported(
                    "response_format must be wav, mp3, opus or pcm.",
                ));
            }
        };
        match fields.get("stream_format") {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) if value == "audio" => {}
            _ => return Err(unsupported("Only complete audio responses are supported.")),
        }
        let speed = match fields.get("speed") {
            None | Some(Value::Null) => 1.0,
            Some(value) => value
                .as_f64()
                .filter(|value| value.is_finite() && (0.25..=4.0).contains(value))
                .ok_or_else(|| invalid(CODE, "speed must be a number from 0.25 to 4."))?,
        };
        Ok(Self {
            model,
            input,
            voice,
            format,
            speed,
        })
    }

    pub fn upstream(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "model": "kokoro",
            "input": self.input,
            "voice": self.voice,
            "response_format": self.format.name(),
            "speed": self.speed,
            "stream": false,
        }))
        .expect("speech request serializes")
    }
}

fn upstream_error(code: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        code,
        "The media worker returned a malformed or oversized response.",
    )
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_value(byte: u8) -> Option<u8> {
    B64.iter()
        .position(|known| *known == byte)
        .map(|index| index as u8)
}

/// Checks the whole alphabet and padding, then decodes only the leading magic bytes.
pub fn b64_prefix(encoded: &str, bytes: usize) -> Option<Vec<u8>> {
    let raw = encoded.as_bytes();
    if raw.is_empty() || !raw.len().is_multiple_of(4) {
        return None;
    }
    let padding = raw.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2
        || raw[..raw.len() - padding]
            .iter()
            .any(|byte| b64_value(*byte).is_none())
    {
        return None;
    }
    let mut output = Vec::with_capacity(bytes + 3);
    for chunk in raw.chunks(4) {
        if output.len() >= bytes {
            break;
        }
        let values: Vec<u8> = chunk.iter().filter_map(|byte| b64_value(*byte)).collect();
        let word = values
            .iter()
            .enumerate()
            .fold(0u32, |word, (index, value)| {
                word | (u32::from(*value) << (18 - 6 * index))
            });
        for index in 0..values.len().saturating_sub(1) {
            output.push((word >> (16 - 8 * index)) as u8);
        }
    }
    output.truncate(bytes);
    Some(output)
}

fn image_payload(encoded: &str, format: Option<ImageFormat>) -> bool {
    b64_prefix(encoded, 16).is_some_and(|prefix| match format {
        Some(format) => format.matches(&prefix),
        None => [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp]
            .iter()
            .any(|format| format.matches(&prefix)),
    })
}

#[derive(Deserialize)]
struct ImageItem<'a> {
    #[serde(borrow)]
    b64_json: Cow<'a, str>,
}
#[derive(Deserialize)]
struct ImageReply<'a> {
    #[serde(borrow)]
    data: Vec<ImageItem<'a>>,
}
#[derive(Deserialize)]
struct Txt2ImgReply<'a> {
    #[serde(borrow)]
    images: Vec<Cow<'a, str>>,
}

pub fn validate_image_response(bytes: &Bytes, format: ImageFormat) -> Result<(), ApiError> {
    validate_images_response(bytes, format, 1)
}

pub fn validate_images_response(
    bytes: &Bytes,
    format: ImageFormat,
    count: u64,
) -> Result<(), ApiError> {
    if bytes.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(upstream_error("invalid_image_response"));
    }
    let reply: ImageReply<'_> =
        serde_json::from_slice(bytes).map_err(|_| upstream_error("invalid_image_response"))?;
    if reply.data.len() as u64 == count
        && reply
            .data
            .iter()
            .all(|item| image_payload(&item.b64_json, Some(format)))
    {
        Ok(())
    } else {
        Err(upstream_error("invalid_image_response"))
    }
}

pub fn validate_txt2img_response(bytes: &Bytes, count: u64) -> Result<(), ApiError> {
    if bytes.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(upstream_error("invalid_image_response"));
    }
    let reply: Txt2ImgReply<'_> =
        serde_json::from_slice(bytes).map_err(|_| upstream_error("invalid_image_response"))?;
    if reply.images.len() as u64 == count
        && reply.images.iter().all(|image| image_payload(image, None))
    {
        Ok(())
    } else {
        Err(upstream_error("invalid_image_response"))
    }
}

pub fn validate_speech_response(bytes: &Bytes, format: AudioFormat) -> Result<(), ApiError> {
    if bytes.len() > MAX_SPEECH_RESPONSE_BYTES || !format.matches(bytes) {
        return Err(upstream_error("invalid_speech_response"));
    }
    Ok(())
}

pub fn upload_route(path: &str) -> bool {
    matches!(path, "/images/edits" | "/images/variations")
}

type Form = Result<Multipart, axum::extract::multipart::MultipartRejection>;

pub(crate) async fn image_edit(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    form: Form,
) -> Result<Response, ApiError> {
    edit_exchange(&state, &context, &headers, form, false).await
}

pub(crate) async fn image_variation(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    form: Form,
) -> Result<Response, ApiError> {
    edit_exchange(&state, &context, &headers, form, true).await
}

async fn edit_exchange(
    state: &AppState,
    context: &RequestContext,
    headers: &HeaderMap,
    form: Form,
    variation: bool,
) -> Result<Response, ApiError> {
    let priority = crate::http::request_priority(context, headers)?;
    let multipart = form.map_err(|_| {
        invalid(
            "invalid_image_request",
            "Request must be multipart/form-data.",
        )
    })?;
    let request = EditRequest::parse(multipart, variation).await?;
    let form = request.upstream();
    let (bytes, mut lease) = crate::http::media_exchange_with(
        &state.manager,
        &request.model,
        crate::config::Operation::ImageGeneration,
        priority,
        "/v1/images/edits",
        MAX_IMAGE_RESPONSE_BYTES,
        |builder| builder.multipart(form),
    )
    .await?;
    validate_images_response(&bytes, request.format, request.n)
        .inspect_err(|_| lease.finish("upstream_error", Some(200)))?;
    lease.finish("completed", Some(200));
    Ok(crate::http::media_reply("application/json", bytes))
}

/// v0.9.2 upscaled through sd-cli with a registered ESRGAN model. The rewrite registers no
/// upscaling models, so a request reports the same validation outcome without running one.
pub(crate) async fn image_upscale(
    State(state): State<AppState>,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    const CODE: &str = "invalid_upscale_request";
    let fields = object(&body, CODE)?;
    if !fields.get("image").is_some_and(Value::is_string) {
        return Err(invalid(CODE, "Missing 'image' field (base64 encoded)"));
    }
    let model = fields
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| invalid(CODE, "Missing 'model' field"))?;
    let registered = state
        .manager
        .catalog()
        .await
        .iter()
        .any(|entry| entry.id == model);
    Err(if registered {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "upscale_model_required",
            format!("Upscale model is not labeled 'upscaling': {model}"),
        )
    } else {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_model",
            format!("Upscale model not found: {model}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(bytes: &[u8]) -> String {
        let mut output = String::new();
        for chunk in bytes.chunks(3) {
            let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
                word | (u32::from(*byte) << (16 - 8 * index))
            });
            for index in 0..4 {
                if index <= chunk.len() {
                    output.push(B64[((word >> (18 - 6 * index)) & 63) as usize] as char);
                } else {
                    output.push('=');
                }
            }
        }
        output
    }

    #[test]
    fn image_requests_translate_sampling_into_the_tagged_prompt_suffix() {
        let request = ImageRequest::parse(
            br#"{"model":"sd","prompt":"a cabin","size":"768x512","steps":4,"cfg_scale":1.0,"seed":7,"negative_prompt":"blur","response_format":"b64_json","n":1,"user":"u"}"#,
        )
        .unwrap();
        assert_eq!(request.size, Some((768, 512)));
        let upstream: Value = serde_json::from_slice(&request.upstream()).unwrap();
        assert_eq!(upstream["size"], "768x512");
        assert_eq!(upstream["n"], 1);
        assert_eq!(upstream["output_format"], "png");
        let prompt = upstream["prompt"].as_str().unwrap();
        let (text, extra) = prompt.split_once(" <sd_cpp_extra_args>").unwrap();
        assert_eq!(text, "a cabin");
        let extra: Value =
            serde_json::from_str(extra.strip_suffix("</sd_cpp_extra_args>").unwrap()).unwrap();
        assert_eq!(extra["sample_params"]["sample_steps"], 4);
        assert_eq!(extra["sample_params"]["guidance"]["txt_cfg"], 1.0);
        assert_eq!(extra["seed"], 7);
        assert_eq!(extra["negative_prompt"], "blur");
        assert!(upstream.get("model").is_none());
        let plain = ImageRequest::parse(br#"{"model":"sd","prompt":"x"}"#).unwrap();
        let upstream: Value = serde_json::from_slice(&plain.upstream()).unwrap();
        assert_eq!(upstream["prompt"], "x");
        assert!(upstream.get("size").is_none());
    }

    #[test]
    fn image_requests_reject_unbounded_injected_or_unsupported_input() {
        for body in [
            r#"{"prompt":"x"}"#.to_owned(),
            r#"{"model":"sd"}"#.to_owned(),
            r#"{"model":"sd","prompt":"   "}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","n":2}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","response_format":"url"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","size":"4096x4096"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","size":"513x512"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","size":"32x32"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","size":"big"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","steps":0}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","steps":151}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","cfg_scale":51}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","seed":-2}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","sample_method":"euler a"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","quality":"hd"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","image":"AAAA"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x <sd_cpp_extra_args>{}</sd_cpp_extra_args>"}"#.to_owned(),
            r#"{"model":"sd","prompt":"x","negative_prompt":"sd_cpp_extra_args"}"#.to_owned(),
            r#"[]"#.to_owned(),
        ] {
            let error = ImageRequest::parse(body.as_bytes()).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
        }
        let long = json!({"model":"sd","prompt":"x".repeat(MAX_PROMPT_BYTES + 1)}).to_string();
        assert_eq!(
            ImageRequest::parse(long.as_bytes()).unwrap_err().status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn txt2img_requests_keep_only_the_bounded_subset() {
        let request = Txt2ImgRequest::parse(
            br#"{"prompt":"a cabin","width":512,"height":768,"steps":4,"cfg_scale":1,"seed":-1,"sampler_name":"euler_a","batch_size":1}"#,
        )
        .unwrap();
        assert_eq!(request.model, None);
        let upstream: Value = serde_json::from_slice(&request.upstream()).unwrap();
        assert_eq!(upstream["height"], 768);
        assert_eq!(upstream["sampler_name"], "euler_a");
        assert_eq!(upstream["seed"], -1);
        assert!(upstream.get("model").is_none());
        for body in [
            r#"{"prompt":"x","batch_size":5}"#,
            r#"{"prompt":"x","n_iter":2}"#,
            r#"{"prompt":"x","enable_hr":true}"#,
            r#"{"prompt":"x","restore_faces":true}"#,
            r#"{"prompt":"x","send_images":false}"#,
            r#"{"prompt":"x","alwayson_scripts":{"ADetailer":{}}}"#,
            r#"{"prompt":"x","override_settings":{"sd_vae":"x.vae"}}"#,
            r#"{"prompt":"x","override_settings":[]}"#,
            r#"{"prompt":"x","init_images":[]}"#,
            r#"{"prompt":"x","width":4000}"#,
            r#"{"prompt":"x","script_name":"y"}"#,
            r#"{"juiced_health_probe":true}"#,
        ] {
            let error = Txt2ImgRequest::parse(body.as_bytes()).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
        }
        let conflict = Txt2ImgRequest::parse(
            br#"{"prompt":"x","model":"a","override_settings":{"sd_model_checkpoint":"b"}}"#,
        )
        .unwrap_err();
        assert_eq!(conflict.status, StatusCode::CONFLICT);
    }

    #[test]
    fn txt2img_accepts_the_sillytavern_a1111_body() {
        let request = Txt2ImgRequest::parse(
            br#"{"url":"http://juiced:8080","auth":"user:secret","prompt":"a fox","negative_prompt":"blur","sampler_name":"euler_a","scheduler":"N/A","steps":20,"cfg_scale":7,"width":512,"height":512,"restore_faces":false,"enable_hr":false,"hr_upscaler":"Latent","hr_scale":2,"hr_additional_modules":[],"denoising_strength":0.7,"hr_second_pass_steps":0,"override_settings":{"CLIP_stop_at_last_layers":2,"sd_model_checkpoint":"zimage"},"override_settings_restore_afterwards":true,"clip_skip":2,"save_images":true,"send_images":true,"do_not_save_grid":false,"do_not_save_samples":false,"batch_size":2}"#,
        )
        .unwrap();
        assert_eq!(request.model.as_deref(), Some("zimage"));
        assert_eq!(request.batch, 2);
        let upstream: Value = serde_json::from_slice(&request.upstream()).unwrap();
        assert_eq!(
            upstream,
            json!({"prompt":"a fox","negative_prompt":"blur","sampler_name":"euler_a","steps":20,"cfg_scale":7.0,"width":512,"height":512,"clip_skip":2,"batch_size":2})
        );
        let layers = Txt2ImgRequest::parse(
            br#"{"prompt":"x","override_settings":{"CLIP_stop_at_last_layers":3,"forge_additional_modules":[]}}"#,
        )
        .unwrap();
        assert_eq!(layers.body["clip_skip"], 3);
    }

    #[test]
    fn image_defaults_round_trip_through_launch_arguments() {
        let args: Vec<String> = [
            "--listen-port",
            "{port}",
            "--model",
            "{model}",
            "--steps",
            "8",
            "-W",
            "768",
            "--cfg-scale=1",
            "--scheduler",
            "karras",
            "--diffusion-fa",
        ]
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
        let view = image_defaults_view(&args);
        assert_eq!(
            view,
            json!({"steps":8,"cfg_scale":1.0,"width":768,"height":null,"sampling_method":null,"scheduler":"karras","flow_shift":null,"seed":null})
        );
        let patch = json!({"steps":4,"cfg_scale":null,"height":1024,"flow_shift":3.0,"sampling_method":"euler"});
        let next = apply_image_defaults(&args, patch.as_object().unwrap()).unwrap();
        assert_eq!(
            next,
            [
                "--listen-port",
                "{port}",
                "--model",
                "{model}",
                "-W",
                "768",
                "--scheduler",
                "karras",
                "--diffusion-fa",
                "--steps",
                "4",
                "-H",
                "1024",
                "--sampling-method",
                "euler",
                "--flow-shift",
                "3",
            ]
        );
        assert_eq!(
            image_defaults_view(&next),
            json!({"steps":4,"cfg_scale":null,"width":768,"height":1024,"sampling_method":"euler","scheduler":"karras","flow_shift":3.0,"seed":null})
        );
        for patch in [
            json!({"steps":0}),
            json!({"steps":4.5}),
            json!({"width":500}),
            json!({"cfg_scale":51}),
            json!({"seed":-2}),
            json!({"sampling_method":"euler a"}),
            json!({"ctx_size":4}),
        ] {
            let error = apply_image_defaults(&args, patch.as_object().unwrap()).unwrap_err();
            assert_eq!(error.code, "invalid_image_defaults", "{patch}");
        }
    }

    #[test]
    fn image_headers_give_dimensions_and_edits_fit_the_side_limit() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&4000u32.to_be_bytes());
        png.extend_from_slice(&3000u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((4000, 3000)));
        assert_eq!(fitted_size(4000, 3000), (2048, 1536));
        assert_eq!(fitted_size(500, 333), (496, 328));
        assert_eq!(fitted_size(20, 20), (64, 64));
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01,
            0xe0, 0x02, 0x80,
        ];
        assert_eq!(image_dimensions(&jpeg), Some((640, 480)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend_from_slice(&[0xff, 0x03, 0x00, 0xff, 0x01, 0x00]);
        assert_eq!(image_dimensions(&webp), Some((1024, 512)));
        assert_eq!(image_dimensions(b"GIF89a"), None);
        assert_eq!(image_dimensions(b"\x89PNG\r\n\x1a\n"), None);
    }

    #[test]
    fn speech_requests_validate_voice_format_speed_and_size() {
        let request = SpeechRequest::parse(
            br#"{"model":"k","input":"Hello","voice":"af_sarah.4+af_nicole.6","response_format":"wav","speed":1.25}"#,
        )
        .unwrap();
        let upstream: Value = serde_json::from_slice(&request.upstream()).unwrap();
        assert_eq!(upstream["voice"], "af_sarah.4+af_nicole.6");
        assert_eq!(upstream["response_format"], "wav");
        assert_eq!(upstream["stream"], false);
        assert_eq!(upstream["model"], "kokoro");
        let default =
            SpeechRequest::parse(br#"{"model":"k","input":"Hi","voice":"alloy"}"#).unwrap();
        assert_eq!(default.format, AudioFormat::Mp3);
        assert_eq!(default.speed, 1.0);
        for body in [
            r#"{"model":"k","input":"Hi"}"#,
            r#"{"model":"k","voice":"alloy"}"#,
            r#"{"model":"k","input":"Hi","voice":"../x"}"#,
            r#"{"model":"k","input":"Hi","voice":"a.b"}"#,
            r#"{"model":"k","input":"Hi","voice":"alloy","response_format":"flac"}"#,
            r#"{"model":"k","input":"Hi","voice":"alloy","speed":5}"#,
            r#"{"model":"k","input":"Hi","voice":"alloy","stream_format":"sse"}"#,
            r#"{"model":"k","input":"Hi","voice":"alloy","instructions":"calm"}"#,
        ] {
            assert_eq!(
                SpeechRequest::parse(body.as_bytes()).unwrap_err().status,
                StatusCode::BAD_REQUEST,
                "{body}"
            );
        }
        let long =
            json!({"model":"k","voice":"alloy","input":"x".repeat(MAX_SPEECH_INPUT_BYTES + 1)})
                .to_string();
        assert_eq!(
            SpeechRequest::parse(long.as_bytes()).unwrap_err().status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn base64_prefix_decoding_checks_the_whole_alphabet() {
        let png = b"\x89PNG\r\n\x1a\n0000IHDR";
        let encoded = encode(png);
        assert_eq!(b64_prefix(&encoded, 8).unwrap(), &png[..8]);
        assert_eq!(b64_prefix(&encode(b"ab"), 8).unwrap(), b"ab");
        assert!(b64_prefix("abc", 2).is_none());
        assert!(b64_prefix("ab!d", 2).is_none());
        assert!(b64_prefix("a===", 2).is_none());
    }

    #[test]
    fn worker_replies_need_one_matching_image_or_audio_payload() {
        let png = encode(b"\x89PNG\r\n\x1a\nrest-of-image");
        let reply = Bytes::from(
            json!({"created":1,"data":[{"b64_json":png}],"output_format":"png"}).to_string(),
        );
        validate_image_response(&reply, ImageFormat::Png).unwrap();
        assert!(validate_image_response(&reply, ImageFormat::Jpeg).is_err());
        for body in [
            json!({"data":[]}).to_string(),
            json!({"data":[{"b64_json":png},{"b64_json":png}]}).to_string(),
            json!({"data":[{"b64_json":encode(b"not an image at all")}]}).to_string(),
            json!({"error":"private path"}).to_string(),
            "not json".to_owned(),
        ] {
            let error = validate_image_response(&Bytes::from(body), ImageFormat::Png).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
            assert!(!error.message.contains("private"));
        }
        validate_txt2img_response(
            &Bytes::from(json!({"images":[png],"info":"{}"}).to_string()),
            1,
        )
        .unwrap();
        assert!(
            validate_txt2img_response(&Bytes::from(json!({"images":[]}).to_string()), 1).is_err()
        );
        let pair = Bytes::from(json!({"images":[png, png]}).to_string());
        validate_txt2img_response(&pair, 2).unwrap();
        assert!(validate_txt2img_response(&pair, 1).is_err());
        let edits = Bytes::from(json!({"data":[{"b64_json":png},{"b64_json":png}]}).to_string());
        validate_images_response(&edits, ImageFormat::Png, 2).unwrap();
        assert!(validate_images_response(&edits, ImageFormat::Png, 1).is_err());
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.resize(64, 0);
        validate_speech_response(&Bytes::from(wav.clone()), AudioFormat::Wav).unwrap();
        assert!(validate_speech_response(&Bytes::from(wav), AudioFormat::Mp3).is_err());
        validate_speech_response(&Bytes::from_static(b"ID3\x04rest"), AudioFormat::Mp3).unwrap();
        assert!(validate_speech_response(&Bytes::from_static(b"RIFF"), AudioFormat::Wav).is_err());
        assert!(
            validate_speech_response(
                &Bytes::from(vec![0u8; MAX_SPEECH_RESPONSE_BYTES + 2]),
                AudioFormat::Pcm
            )
            .is_err()
        );
    }
}
