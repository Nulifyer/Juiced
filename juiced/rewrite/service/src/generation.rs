use crate::{
    ApiError,
    auth::RequestContext,
    config::{Operation, Provider},
    extract::ApiBytes,
    http::AppState,
    supervisor::{Manager, RequestLease},
};
use axum::{
    Extension,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_LYRICS_BYTES: usize = 16 * 1024;
pub const MAX_AUDIO_RESPONSE_BYTES: usize = 192 * 1024 * 1024;
pub const MAX_MESH_RESPONSE_BYTES: usize = 384 * 1024 * 1024;
pub const MAX_DURATION_SECONDS: f64 = 600.0;
const MAX_STEPS: u64 = 200;
const MAX_CFG: f64 = 50.0;
const MAX_JOB_STATUS_BYTES: usize = 64 * 1024;
const MAX_LM_RESULT_BYTES: usize = 16 * 1024 * 1024;
const JOB_POLL: Duration = Duration::from_secs(1);
/// The native adapters' own upstream deadlines: music and sound generation can take minutes
/// on integrated GPUs and TRELLIS cascades longer, beyond an ordinary request timeout.
const AUDIO_DEADLINE: Duration = Duration::from_secs(20 * 60);
const MESH_DEADLINE: Duration = Duration::from_secs(30 * 60);
const ACE_BOUNDARY: &str = "\r\n--ace-batch-boundary";

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_generation_request",
        message.into(),
    )
}

fn object(body: &[u8]) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(invalid("Send one JSON object.")),
    }
}

fn model(fields: &Map<String, Value>, missing: &str) -> Result<String, ApiError> {
    match fields.get("model") {
        Some(Value::String(model))
            if !model.trim().is_empty()
                && model.len() <= 256
                && !model.chars().any(char::is_control) =>
        {
            Ok(model.clone())
        }
        None | Some(Value::Null) => Err(invalid(missing)),
        _ => Err(invalid("Missing or non-string 'model' field in request")),
    }
}

fn string(
    fields: &Map<String, Value>,
    key: &str,
    limit: usize,
) -> Result<Option<String>, ApiError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= limit => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(invalid(format!("'{key}' is longer than {limit} bytes"))),
        _ => Err(invalid(format!("'{key}' must be a string"))),
    }
}

fn number(
    fields: &Map<String, Value>,
    key: &str,
    range: std::ops::RangeInclusive<f64>,
) -> Result<Option<f64>, ApiError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite() && range.contains(value))
            .map(Some)
            .ok_or_else(|| {
                invalid(format!(
                    "'{key}' must be a number from {} to {}",
                    range.start(),
                    range.end()
                ))
            }),
    }
}

fn integer(
    fields: &Map<String, Value>,
    key: &str,
    range: std::ops::RangeInclusive<i64>,
) -> Result<Option<i64>, ApiError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|value| range.contains(value))
            .map(Some)
            .ok_or_else(|| {
                invalid(format!(
                    "'{key}' must be an integer from {} to {}",
                    range.start(),
                    range.end()
                ))
            }),
    }
}

/// `POST /audio/generations`: a text prompt to music (ACE-Step), sound effects (ThinkSound,
/// MOSS-SoundEffect), returned as WAV bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioRequest {
    pub model: String,
    pub prompt: String,
    pub lyrics: Option<String>,
    pub vocal_language: Option<String>,
    pub description: Option<String>,
    pub negative_prompt: Option<String>,
    pub duration: Option<f64>,
    pub steps: Option<i64>,
    pub cfg: Option<f64>,
    pub seed: Option<i64>,
}

const AUDIO_FIELDS: &[&str] = &[
    "model",
    "prompt",
    "lyrics",
    "vocal_language",
    "description",
    "cot",
    "negative_prompt",
    "duration",
    "seconds",
    "steps",
    "cfg",
    "cfg_scale",
    "seed",
    "response_format",
];

fn reject_unknown(fields: &Map<String, Value>, known: &[&str]) -> Result<(), ApiError> {
    match fields.keys().find(|key| !known.contains(&key.as_str())) {
        Some(key) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            format!("'{key}' is not a supported parameter for this endpoint"),
        )),
        None => Ok(()),
    }
}

fn first_number(
    fields: &Map<String, Value>,
    keys: [&str; 2],
    range: std::ops::RangeInclusive<f64>,
) -> Result<Option<f64>, ApiError> {
    Ok(match number(fields, keys[0], range.clone())? {
        Some(value) => Some(value),
        None => number(fields, keys[1], range)?,
    })
}

impl AudioRequest {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        let fields = object(body)?;
        let model = model(&fields, "Missing 'model' field in request")?;
        if !fields.contains_key("prompt") {
            return Err(invalid("Missing 'prompt' field in request"));
        }
        reject_unknown(&fields, AUDIO_FIELDS)?;
        let prompt = string(&fields, "prompt", MAX_PROMPT_BYTES)?
            .filter(|prompt| !prompt.trim().is_empty())
            .ok_or_else(|| invalid("'prompt' must be a nonempty string"))?;
        let lyrics = string(&fields, "lyrics", MAX_LYRICS_BYTES)?;
        let vocal_language = string(&fields, "vocal_language", 16)?;
        if vocal_language.as_deref().is_some_and(|language| {
            language.is_empty()
                || !language
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }) {
            return Err(invalid(
                "'vocal_language' must be a BCP-47 language code such as en",
            ));
        }
        let description = match string(&fields, "description", MAX_PROMPT_BYTES)? {
            Some(description) => Some(description),
            None => string(&fields, "cot", MAX_PROMPT_BYTES)?,
        };
        let negative_prompt = string(&fields, "negative_prompt", MAX_PROMPT_BYTES)?;
        let duration = first_number(&fields, ["duration", "seconds"], 0.0..=MAX_DURATION_SECONDS)?;
        if duration == Some(0.0) {
            return Err(invalid("'duration' must be greater than 0"));
        }
        let steps = integer(&fields, "steps", 1..=MAX_STEPS as i64)?;
        let cfg = first_number(&fields, ["cfg", "cfg_scale"], 0.0..=MAX_CFG)?;
        let seed = integer(&fields, "seed", -1..=i64::from(u32::MAX))?;
        match fields.get("response_format") {
            None | Some(Value::Null) => {}
            Some(Value::String(format)) if format == "wav" => {}
            Some(Value::String(format)) => {
                return Err(invalid(format!(
                    "response_format '{format}' is not supported by this model (supported: wav)"
                )));
            }
            _ => return Err(invalid("'response_format' must be a string")),
        }
        Ok(Self {
            model,
            prompt,
            lyrics,
            vocal_language,
            description,
            negative_prompt,
            duration,
            steps,
            cfg,
            seed,
        })
    }

    /// Lyrics that are empty or the `[Instrumental]` sentinel select the instrumental path.
    pub fn vocal(&self) -> bool {
        self.lyrics.as_deref().is_some_and(|lyrics| {
            let lyrics = lyrics.trim();
            !lyrics.is_empty() && !lyrics.eq_ignore_ascii_case("[instrumental]")
        })
    }

    pub fn acestep(&self) -> Value {
        let mut body = json!({
            "caption": self.prompt,
            "synth_model": "",
            "output_format": "wav24",
        });
        if let Some(duration) = self.duration {
            body["duration"] = json!(duration);
        }
        if let Some(seed) = self.seed {
            body["seed"] = json!(seed);
        }
        if let Some(steps) = self.steps {
            body["inference_steps"] = json!(steps);
        }
        if self.vocal() {
            body["lyrics"] = json!(self.lyrics);
            body["lm_mode"] = json!("generate");
            body["vocal_language"] = json!(self.vocal_language.as_deref().unwrap_or("en"));
        } else {
            body["lyrics"] = json!("[Instrumental]");
        }
        body
    }

    pub fn thinksound(&self) -> Value {
        let mut body = json!({
            "caption": self.prompt,
            "description": self.description.as_deref().unwrap_or(&self.prompt),
        });
        if let Some(duration) = self.duration {
            body["duration"] = json!(duration);
        }
        if let Some(steps) = self.steps {
            body["steps"] = json!(steps);
        }
        if let Some(cfg) = self.cfg {
            body["cfg"] = json!(cfg);
        }
        if let Some(seed) = self.seed {
            body["seed"] = json!(seed);
        }
        body
    }

    pub fn openmoss(&self) -> Value {
        let mut body = json!({"prompt": self.prompt, "response_format": "wav"});
        if let Some(duration) = self.duration {
            body["seconds"] = json!(duration);
        }
        if let Some(steps) = self.steps {
            body["steps"] = json!(steps);
        }
        if let Some(cfg) = self.cfg {
            body["cfg_scale"] = json!(cfg);
        }
        if let Some(seed) = self.seed {
            body["seed"] = json!(seed);
        }
        if let Some(negative) = &self.negative_prompt {
            body["negative_prompt"] = json!(negative);
        }
        body
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Bmp,
    Gif,
}

impl ImageKind {
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"BM") {
            Some(Self::Bmp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Bmp => "image/bmp",
            Self::Gif => "image/gif",
        }
    }
}

/// `POST /3d/generations`: one input image to a textured glTF-binary mesh (TRELLIS).
#[derive(Clone, Debug, PartialEq)]
pub struct MeshRequest {
    pub model: String,
    pub image: Vec<u8>,
    pub kind: ImageKind,
    pub resolution: Option<u32>,
    pub bg_removal: Option<String>,
    pub uv: Option<String>,
    pub seed: Option<i64>,
}

const MESH_FIELDS: &[&str] = &[
    "model",
    "image",
    "resolution",
    "bg_removal",
    "uv",
    "seed",
    "response_format",
];

fn choice(
    fields: &Map<String, Value>,
    key: &str,
    allowed: &[&str],
) -> Result<Option<String>, ApiError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if allowed.contains(&value.as_str()) => Ok(Some(value.clone())),
        _ => Err(invalid(format!(
            "'{key}' must be {}",
            allowed
                .iter()
                .map(|value| format!("'{value}'"))
                .collect::<Vec<_>>()
                .join(" or ")
        ))),
    }
}

impl MeshRequest {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        use base64::Engine as _;
        let fields = object(body)?;
        let model = model(&fields, "Missing or non-string 'model' field in request")?;
        let Some(Value::String(image)) = fields.get("image") else {
            return Err(invalid(
                "Missing or non-string 'image' field in request (base64-encoded input image)",
            ));
        };
        reject_unknown(&fields, MESH_FIELDS)?;
        let encoded = match image.strip_prefix("data:") {
            Some(url) => {
                url.split_once(',')
                    .ok_or_else(|| invalid("'image' data URL is missing its base64 payload"))?
                    .1
            }
            None => image.as_str(),
        };
        let compact: String = encoded
            .chars()
            .filter(|character| !matches!(character, '\r' | '\n'))
            .collect();
        let image = base64::engine::general_purpose::STANDARD
            .decode(compact.as_bytes())
            .ok()
            .filter(|image| !image.is_empty())
            .ok_or_else(|| invalid("'image' must be base64 data or a base64 data: URL"))?;
        let kind = ImageKind::sniff(&image).ok_or_else(|| {
            invalid("'image' is not a supported format (expected PNG, JPEG, BMP, or GIF)")
        })?;
        match fields.get("response_format") {
            None | Some(Value::Null) => {}
            Some(Value::String(format)) if format == "glb" => {}
            Some(Value::String(format)) => {
                return Err(invalid(format!(
                    "response_format '{format}' is not supported (supported: glb)"
                )));
            }
            _ => return Err(invalid("'response_format' must be a string")),
        }
        let resolution = match fields.get("resolution") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let text = match value {
                    Value::String(text) => Some(text.clone()),
                    Value::Number(number) if number.is_u64() => Some(number.to_string()),
                    _ => None,
                };
                match text.as_deref() {
                    Some("512") => Some(512),
                    Some("1024") => Some(1024),
                    Some("1536") => Some(1536),
                    _ => return Err(invalid("'resolution' must be 512, 1024, or 1536")),
                }
            }
        };
        let bg_removal = choice(&fields, "bg_removal", &["threshold", "birefnet"])?;
        let uv = choice(&fields, "uv", &["box", "xatlas"])?;
        let seed = match fields.get("seed") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .filter(|seed| (0..=i64::from(u32::MAX)).contains(seed))
                    .ok_or_else(|| invalid("'seed' must be an integer"))?,
            ),
        };
        Ok(Self {
            model,
            image,
            kind,
            resolution,
            bg_removal,
            uv,
            seed,
        })
    }

    pub fn form(&self) -> reqwest::multipart::Form {
        let mut form = reqwest::multipart::Form::new().part(
            "image",
            reqwest::multipart::Part::bytes(self.image.clone())
                .file_name(format!("input.{}", self.kind.extension()))
                .mime_str(self.kind.mime())
                .expect("static image mime"),
        );
        if let Some(seed) = self.seed {
            form = form.text("seed", seed.to_string());
        }
        if let Some(resolution) = self.resolution {
            form = form.text("resolution", resolution.to_string());
        }
        if let Some(mode) = &self.bg_removal {
            form = form.text("bg_removal", mode.clone());
        }
        if let Some(uv) = &self.uv {
            form = form.text("uv", uv.clone());
        }
        form
    }
}

pub fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

pub fn is_glb(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[..4] == b"glTF"
        && u32::from_le_bytes(bytes[4..8].try_into().expect("version")) == 2
}

/// ace-server returns a finished synth job as `multipart/mixed` with the audio part first.
pub fn acestep_audio(body: &[u8]) -> Option<&[u8]> {
    let header = find(body, b"Content-Type: audio/")?;
    let start = header + find(&body[header..], b"\r\n\r\n")? + 4;
    let end = start + find(&body[start..], ACE_BOUNDARY.as_bytes())?;
    Some(&body[start..end])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn not_applicable(model: &str, kind: &str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "model_not_applicable",
        format!("Model '{model}' is not {kind} model"),
    )
}

async fn begin(
    manager: &Arc<Manager>,
    model: &str,
    operation: Operation,
    priority: crate::admission::Priority,
    kind: &str,
) -> Result<(String, RequestLease), ApiError> {
    manager
        .begin_queued(model, operation, priority)
        .await
        .map_err(|error| {
            if error.code == "unsupported_operation" {
                not_applicable(model, kind)
            } else {
                error
            }
        })
}

fn transfer_error(lease: &mut RequestLease, error: reqwest::Error) -> ApiError {
    let timeout = error.is_timeout();
    lease.finish(if timeout { "timeout" } else { "upstream_error" }, None);
    ApiError::new(
        if timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        },
        if timeout {
            "upstream_timeout"
        } else {
            "upstream_unavailable"
        },
        "The generation worker transfer failed. Retry or inspect the selected worker.",
    )
}

fn rejected(lease: &mut RequestLease, status: StatusCode, detail: &str) -> ApiError {
    lease.finish("upstream_error", Some(status.as_u16()));
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "backend_error",
        format!("{detail} (HTTP {})", status.as_u16()),
    )
}

fn empty_output(lease: &mut RequestLease) -> ApiError {
    lease.finish("upstream_error", Some(200));
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "backend_error",
        "Generation failed: the backend produced no output (it likely crashed or ran out of GPU memory). Check the server logs.",
    )
}

async fn read_bounded(
    lease: &mut RequestLease,
    mut upstream: reqwest::Response,
    limit: usize,
) -> Result<(StatusCode, Bytes), ApiError> {
    let status = upstream.status();
    let mut output = Vec::with_capacity(
        upstream
            .content_length()
            .unwrap_or(0)
            .min(limit as u64)
            .min(4 * 1024 * 1024) as usize,
    );
    while let Some(chunk) = upstream
        .chunk()
        .await
        .map_err(|error| transfer_error(lease, error))?
    {
        if output.len().saturating_add(chunk.len()) > limit {
            lease.finish("upstream_error", Some(status.as_u16()));
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "media_response_limit",
                "The generation worker response exceeded its bounded result size.",
            ));
        }
        output.extend_from_slice(&chunk);
    }
    Ok((status, Bytes::from(output)))
}

fn deadline(manager: &Manager, native: Duration) -> Duration {
    manager.request_timeout.max(native)
}

/// Submits one ace-server job and waits for its result. The worker keeps the job after a
/// client disconnect, so dropping this future retires the worker generation.
async fn acestep_job(
    manager: &Arc<Manager>,
    lease: &mut RequestLease,
    base: &str,
    path: &str,
    body: Vec<u8>,
    until: Instant,
    limit: usize,
) -> Result<Bytes, ApiError> {
    let stage = &path[1..];
    let remaining = |until: Instant| until.saturating_duration_since(Instant::now());
    let submit = manager
        .client
        .post(format!("{base}{path}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .timeout(remaining(until).min(Duration::from_secs(60)))
        .send()
        .await
        .map_err(|error| transfer_error(lease, error))?;
    let (status, reply) = read_bounded(lease, submit, MAX_JOB_STATUS_BYTES).await?;
    if !status.is_success() {
        return Err(rejected(lease, status, &format!("{stage} submit failed")));
    }
    let id = serde_json::from_slice::<Value>(&reply)
        .ok()
        .and_then(|reply| reply["id"].as_str().map(str::to_owned))
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        })
        .ok_or_else(|| {
            lease.finish("upstream_error", Some(status.as_u16()));
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backend_error",
                format!("{stage} submit returned no job id"),
            )
        })?;
    let job = format!("{base}/job?id={id}");
    let mut state = String::new();
    while Instant::now() < until {
        if let Ok(poll) = manager
            .client
            .get(&job)
            .timeout(remaining(until).min(Duration::from_secs(30)))
            .send()
            .await
            && poll.status().is_success()
            && let Ok((_, reply)) = read_bounded(lease, poll, MAX_JOB_STATUS_BYTES).await
        {
            state = serde_json::from_slice::<Value>(&reply)
                .ok()
                .and_then(|reply| reply["status"].as_str().map(str::to_owned))
                .unwrap_or_default();
        }
        if matches!(state.as_str(), "done" | "failed" | "cancelled") {
            break;
        }
        tokio::time::sleep(JOB_POLL.min(remaining(until))).await;
    }
    if state != "done" {
        lease.finish(
            if state.is_empty() || !matches!(state.as_str(), "failed" | "cancelled") {
                "timeout"
            } else {
                "upstream_error"
            },
            None,
        );
        return Err(ApiError::new(
            if matches!(state.as_str(), "failed" | "cancelled") {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::GATEWAY_TIMEOUT
            },
            "backend_error",
            format!("{stage} job ended with status '{state}'"),
        ));
    }
    let fetched = manager
        .client
        .get(format!("{job}&result=1"))
        .timeout(remaining(until).min(Duration::from_secs(120)))
        .send()
        .await
        .map_err(|error| transfer_error(lease, error))?;
    let (status, result) = read_bounded(lease, fetched, limit).await?;
    if !status.is_success() {
        return Err(rejected(
            lease,
            status,
            &format!("fetching {stage} job result failed"),
        ));
    }
    Ok(result)
}

async fn post_json(
    manager: &Arc<Manager>,
    lease: &mut RequestLease,
    url: String,
    body: &Value,
    timeout: Duration,
    limit: usize,
) -> Result<Bytes, ApiError> {
    let upstream = manager
        .client
        .post(url)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| transfer_error(lease, error))?;
    let (status, bytes) = read_bounded(lease, upstream, limit).await?;
    if !status.is_success() {
        return Err(rejected(lease, status, "generation failed"));
    }
    Ok(bytes)
}

fn audio_reply(bytes: Bytes) -> Response {
    (
        [
            (header::CONTENT_TYPE, "audio/wav"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response()
}

pub(crate) async fn audio_generation(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let priority = crate::http::request_priority(&context, &headers)?;
    let request = AudioRequest::parse(&body)?;
    let manager = &state.manager;
    let (base, mut lease) = begin(
        manager,
        &request.model,
        Operation::AudioGeneration,
        priority,
        "an audio-generation",
    )
    .await?;
    lease.speech_dispatched();
    let timeout = deadline(manager, AUDIO_DEADLINE);
    let audio = match lease.adapter() {
        Provider::AceStep => {
            let until = Instant::now() + timeout;
            let mut synth = request.acestep();
            if request.vocal() {
                let planned = acestep_job(
                    manager,
                    &mut lease,
                    &base,
                    "/lm",
                    serde_json::to_vec(&synth).expect("lm request"),
                    until,
                    MAX_LM_RESULT_BYTES,
                )
                .await?;
                synth = serde_json::from_slice::<Value>(&planned)
                    .ok()
                    .filter(|planned| planned.is_object() || planned.is_array())
                    .ok_or_else(|| {
                        lease.finish("upstream_error", Some(200));
                        ApiError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "backend_error",
                            "lm job returned no synth request",
                        )
                    })?;
            }
            let result = acestep_job(
                manager,
                &mut lease,
                &base,
                "/synth",
                serde_json::to_vec(&synth).expect("synth request"),
                until,
                MAX_AUDIO_RESPONSE_BYTES,
            )
            .await?;
            let audio = acestep_audio(&result).ok_or_else(|| {
                lease.finish("upstream_error", Some(200));
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "backend_error",
                    "no audio part in synth result",
                )
            })?;
            result.slice_ref(audio)
        }
        Provider::ThinkSound => {
            post_json(
                manager,
                &mut lease,
                format!("{base}/generate"),
                &request.thinksound(),
                timeout,
                MAX_AUDIO_RESPONSE_BYTES,
            )
            .await?
        }
        Provider::OpenMoss => {
            post_json(
                manager,
                &mut lease,
                format!("{base}/sfx"),
                &request.openmoss(),
                timeout,
                MAX_AUDIO_RESPONSE_BYTES,
            )
            .await?
        }
        _ => {
            lease.finish("rejected", Some(400));
            return Err(not_applicable(&request.model, "an audio-generation"));
        }
    };
    lease.speech_reply_eof();
    if audio.is_empty() {
        return Err(empty_output(&mut lease));
    }
    if !is_wav(&audio) {
        lease.finish("upstream_error", Some(200));
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "backend_error",
            "The audio-generation worker returned something other than WAV audio.",
        ));
    }
    lease.finish("completed", Some(200));
    Ok(audio_reply(audio))
}

pub(crate) async fn model_3d_generation(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let priority = crate::http::request_priority(&context, &headers)?;
    let request = MeshRequest::parse(&body)?;
    drop(body);
    let manager = &state.manager;
    let (base, mut lease) = begin(
        manager,
        &request.model,
        Operation::Model3dGeneration,
        priority,
        "a 3D-generation",
    )
    .await?;
    if lease.adapter() != Provider::Trellis {
        lease.finish("rejected", Some(400));
        return Err(not_applicable(&request.model, "a 3D-generation"));
    }
    lease.speech_dispatched();
    let upstream = manager
        .client
        .post(format!("{base}/generate"))
        .multipart(request.form())
        .timeout(deadline(manager, MESH_DEADLINE))
        .send()
        .await
        .map_err(|error| transfer_error(&mut lease, error))?;
    let (status, mesh) = read_bounded(&mut lease, upstream, MAX_MESH_RESPONSE_BYTES).await?;
    lease.speech_reply_eof();
    if !status.is_success() {
        return Err(rejected(&mut lease, status, "generation failed"));
    }
    if mesh.is_empty() {
        return Err(empty_output(&mut lease));
    }
    if !is_glb(&mesh) {
        lease.finish("upstream_error", Some(200));
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "backend_error",
            "The 3D worker returned something other than a glTF-binary mesh.",
        ));
    }
    lease.finish("completed", Some(200));
    Ok((
        [
            (header::CONTENT_TYPE, "model/gltf-binary"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        mesh,
    )
        .into_response())
}

/// `POST /audio/speech` for an OpenMOSS checkpoint: moss-tts-server reads OpenAI fields but
/// renders only WAV or PCM, and VoiceGen checkpoints take a free-text voice description.
#[derive(Clone, Debug, PartialEq)]
pub struct MossSpeech {
    pub model: String,
    pub input: String,
    pub voice: Option<String>,
    pub instruction: Option<String>,
    pub format: &'static str,
    pub speed: Option<f64>,
}

const MOSS_SPEECH_FIELDS: &[&str] = &[
    "model",
    "input",
    "voice",
    "instructions",
    "response_format",
    "speed",
    "stream_format",
];

impl MossSpeech {
    pub fn parse(body: &[u8]) -> Result<Self, ApiError> {
        const CODE: &str = "invalid_speech_request";
        let speech = |message: String| ApiError::new(StatusCode::BAD_REQUEST, CODE, message);
        let fields = object(body).map_err(|error| speech(error.message))?;
        reject_unknown(&fields, MOSS_SPEECH_FIELDS)?;
        let model = model(&fields, "A registered speech model ID is required.")
            .map_err(|error| speech(error.message))?;
        let input = string(&fields, "input", crate::media::MAX_SPEECH_INPUT_BYTES)
            .map_err(|error| speech(error.message))?
            .filter(|input| !input.trim().is_empty())
            .ok_or_else(|| speech("A nonempty input is required.".into()))?;
        let voice = string(&fields, "voice", 1024)
            .map_err(|error| speech(error.message))?
            .filter(|voice| !voice.trim().is_empty());
        let instruction = string(&fields, "instructions", 4096)
            .map_err(|error| speech(error.message))?
            .filter(|instruction| !instruction.trim().is_empty());
        let format = match fields.get("response_format") {
            None | Some(Value::Null) => "wav",
            Some(Value::String(format)) if format == "wav" => "wav",
            Some(Value::String(format)) if format == "pcm" => "pcm",
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_parameter",
                    "OpenMOSS models return wav or pcm audio.",
                ));
            }
        };
        match fields.get("stream_format") {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) if value == "audio" => {}
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_parameter",
                    "Only complete audio responses are supported.",
                ));
            }
        }
        let speed = number(&fields, "speed", 0.25..=4.0).map_err(|error| speech(error.message))?;
        Ok(Self {
            model,
            input,
            voice,
            instruction,
            format,
            speed,
        })
    }

    pub fn upstream(&self) -> Value {
        let mut body = json!({"input": self.input, "response_format": self.format});
        if let Some(voice) = &self.voice {
            body["voice"] = json!(voice);
        }
        if let Some(instruction) = &self.instruction {
            body["instruction"] = json!(instruction);
        }
        if let Some(speed) = self.speed {
            body["speed"] = json!(speed);
        }
        body
    }
}

/// Serves the request when its model runs on OpenMOSS; `None` leaves it to the Kokoros path.
pub async fn openmoss_speech(
    manager: &Arc<Manager>,
    priority: crate::admission::Priority,
    body: &[u8],
) -> Option<Result<Response, ApiError>> {
    let model = serde_json::from_slice::<Value>(body).ok()?["model"]
        .as_str()
        .map(str::to_owned)?;
    if manager.model_adapter(&model) != Some(Provider::OpenMoss) {
        return None;
    }
    Some(
        async {
            let request = MossSpeech::parse(body)?;
            let (base, mut lease) = manager
                .begin_queued(&request.model, Operation::Speech, priority)
                .await?;
            lease.speech_dispatched();
            let audio = post_json(
                manager,
                &mut lease,
                format!("{base}/v1/audio/speech"),
                &request.upstream(),
                deadline(manager, AUDIO_DEADLINE),
                crate::media::MAX_SPEECH_RESPONSE_BYTES,
            )
            .await?;
            lease.speech_reply_eof();
            let valid = match request.format {
                "wav" => is_wav(&audio),
                _ => !audio.is_empty() && audio.len().is_multiple_of(2),
            };
            if !valid {
                lease.finish("upstream_error", Some(200));
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "speech_response_invalid",
                    "The speech worker returned audio in an unexpected format.",
                ));
            }
            lease.finish("completed", Some(200));
            Ok(crate::http::media_reply(
                if request.format == "wav" {
                    "audio/wav"
                } else {
                    "audio/pcm"
                },
                audio,
            ))
        }
        .await,
    )
}

/// TheNoise has no OpenAI image route: `/text2image` takes its own fields and answers one PNG.
pub fn thenoise_body(request: &crate::media::ImageRequest) -> Value {
    let mut body = json!({"prompt": request.prompt, "out": "json"});
    if let Some((width, height)) = request.size {
        body["width"] = json!(width);
        body["height"] = json!(height);
    }
    let sampling = &request.sampling;
    if let Some(negative) = &sampling.negative_prompt {
        body["negative_prompt"] = json!(negative);
    }
    if let Some(steps) = sampling.steps {
        body["steps"] = json!(steps);
    }
    if let Some(cfg) = sampling.cfg_scale {
        body["guidance_scale"] = json!(cfg);
    }
    if let Some(seed) = sampling.seed {
        body["seed"] = json!(seed);
    }
    if let Some(sampler) = &sampling.sampler {
        body["sampler"] = json!(sampler);
    }
    body
}

/// Serves `/images/generations` when its model runs on TheNoise; `None` leaves it to sd.cpp.
pub async fn thenoise_image(
    manager: &Arc<Manager>,
    priority: crate::admission::Priority,
    body: &[u8],
) -> Option<Result<Response, ApiError>> {
    let model = serde_json::from_slice::<Value>(body).ok()?["model"]
        .as_str()
        .map(str::to_owned)?;
    if manager.model_adapter(&model) != Some(Provider::TheNoise) {
        return None;
    }
    Some(
        async {
            let request = crate::media::ImageRequest::parse(body)?;
            if request.format != crate::media::ImageFormat::Png {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_parameter",
                    "TheNoise models return PNG images.",
                ));
            }
            let (base, mut lease) = manager
                .begin_queued(&request.model, Operation::ImageGeneration, priority)
                .await?;
            lease.speech_dispatched();
            let reply = post_json(
                manager,
                &mut lease,
                format!("{base}/text2image"),
                &thenoise_body(&request),
                deadline(manager, AUDIO_DEADLINE),
                crate::media::MAX_IMAGE_RESPONSE_BYTES,
            )
            .await?;
            lease.speech_reply_eof();
            let image = serde_json::from_slice::<Value>(&reply)
                .ok()
                .and_then(|reply| reply["b64_json"].as_str().map(str::to_owned));
            let created = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let wrapped = Bytes::from(
                serde_json::to_vec(&json!({"created": created, "data": [{"b64_json": image}]}))
                    .expect("image reply serializes"),
            );
            if image.is_none()
                || crate::media::validate_image_response(&wrapped, request.format).is_err()
            {
                lease.finish("upstream_error", Some(200));
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "invalid_image_response",
                    "The image worker returned no PNG image.",
                ));
            }
            lease.finish("completed", Some(200));
            Ok(crate::http::media_reply("application/json", wrapped))
        }
        .await,
    )
}

#[cfg(test)]
mod tests;
