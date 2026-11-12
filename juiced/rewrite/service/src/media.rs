use crate::ApiError;
use axum::{body::Bytes, http::StatusCode};
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
const MAX_SEED: i64 = u32::MAX as i64;
const EXTRA_ARGS: &str = "sd_cpp_extra_args";

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

fn dimension(value: u64, code: &'static str) -> Result<u32, ApiError> {
    u32::try_from(value)
        .ok()
        .filter(|value| {
            (MIN_IMAGE_SIDE..=MAX_IMAGE_SIDE).contains(value) && value.is_multiple_of(8)
        })
        .ok_or_else(|| {
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
        let extra = self.sampling.extra_arguments();
        let prompt = if extra.as_object().is_some_and(Map::is_empty) {
            self.prompt.clone()
        } else {
            format!("{} <{EXTRA_ARGS}>{extra}</{EXTRA_ARGS}>", self.prompt)
        };
        let mut body = json!({
            "prompt": prompt,
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
];

impl Txt2ImgRequest {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        const CODE: &str = "invalid_txt2img_request";
        let fields = object(body, CODE)?;
        reject_unknown(
            &fields,
            SDAPI_FIELDS,
            "Use only the supported txt2img subset; scripts, LoRAs, high-resolution passes and image inputs are unavailable.",
        )?;
        let model = model(&fields, CODE)?;
        for key in ["batch_size", "n_iter"] {
            if fields
                .get(key)
                .is_some_and(|value| !value.is_null() && value.as_u64() != Some(1))
            {
                return Err(unsupported(
                    "Only one image per txt2img request is supported.",
                ));
            }
        }
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
            None | Some(Value::Null) => None,
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
        body.insert("batch_size".into(), json!(1));
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
        Ok(Self { model, body })
    }

    pub fn upstream(&self) -> Vec<u8> {
        serde_json::to_vec(&self.body).expect("txt2img request serializes")
    }
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
    if bytes.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(upstream_error("invalid_image_response"));
    }
    let reply: ImageReply<'_> =
        serde_json::from_slice(bytes).map_err(|_| upstream_error("invalid_image_response"))?;
    match reply.data.as_slice() {
        [item] if image_payload(&item.b64_json, Some(format)) => Ok(()),
        _ => Err(upstream_error("invalid_image_response")),
    }
}

pub fn validate_txt2img_response(bytes: &Bytes) -> Result<(), ApiError> {
    if bytes.len() > MAX_IMAGE_RESPONSE_BYTES {
        return Err(upstream_error("invalid_image_response"));
    }
    let reply: Txt2ImgReply<'_> =
        serde_json::from_slice(bytes).map_err(|_| upstream_error("invalid_image_response"))?;
    match reply.images.as_slice() {
        [image] if image_payload(image, None) => Ok(()),
        _ => Err(upstream_error("invalid_image_response")),
    }
}

pub fn validate_speech_response(bytes: &Bytes, format: AudioFormat) -> Result<(), ApiError> {
    if bytes.len() > MAX_SPEECH_RESPONSE_BYTES || !format.matches(bytes) {
        return Err(upstream_error("invalid_speech_response"));
    }
    Ok(())
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
            r#"{"prompt":"x","batch_size":2}"#,
            r#"{"prompt":"x","enable_hr":true}"#,
            r#"{"prompt":"x","init_images":[]}"#,
            r#"{"prompt":"x","width":4000}"#,
            r#"{"prompt":"x","script_name":"y"}"#,
        ] {
            assert!(Txt2ImgRequest::parse(body.as_bytes()).is_err(), "{body}");
        }
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
        validate_txt2img_response(&Bytes::from(
            json!({"images":[png],"info":"{}"}).to_string(),
        ))
        .unwrap();
        assert!(validate_txt2img_response(&Bytes::from(json!({"images":[]}).to_string())).is_err());
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
