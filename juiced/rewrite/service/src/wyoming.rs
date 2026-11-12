//! Wyoming 1.x speech server for Home Assistant. The protocol has no authentication, so the
//! listener is disabled by default and belongs only on trusted networks.

use crate::{ApiError, audio, config::Operation, logs::Level, supervisor::Manager};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};

pub const DEFAULT_LISTEN: &str = "0.0.0.0:10300";
pub const PROTOCOL_VERSION: &str = "1.10.2";
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_DATA_BYTES: usize = 64 * 1024;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const SAMPLES_PER_CHUNK: usize = 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub stt_model: Option<String>,
    #[serde(default)]
    pub tts_model: Option<String>,
    #[serde(default)]
    pub tts_voice: Option<String>,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_listen(),
            stt_model: None,
            tts_model: None,
            tts_voice: None,
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    /// Native containers enable Wyoming with `JUICED_WYOMING_ENABLED=true`; that switch applies
    /// until the operator saves explicit Wyoming settings.
    pub fn from_environment(
        enabled: Option<&str>,
        uri: Option<&str>,
    ) -> Option<Result<Self, &'static str>> {
        if enabled? != "true" {
            return None;
        }
        let listen = match uri {
            None => default_listen(),
            Some(uri) => uri.strip_prefix("tcp://").unwrap_or(uri).to_owned(),
        };
        let settings = Self {
            enabled: true,
            listen,
            ..Self::default()
        };
        Some(settings.validate().map(|()| settings))
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.listen.parse::<SocketAddr>().is_err() {
            return Err("listen must be an IP socket address such as 0.0.0.0:10300.");
        }
        for model in [&self.stt_model, &self.tts_model].into_iter().flatten() {
            if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
                return Err("Models must be null or bounded registered model IDs.");
            }
        }
        if self
            .tts_voice
            .as_deref()
            .is_some_and(|voice| !crate::media::valid_voice_selection(voice))
        {
            return Err(
                "tts_voice must be null, a voice name, or a blend like af_sarah.4+af_nicole.6.",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Event {
    pub kind: String,
    pub data: Map<String, Value>,
    pub payload: Vec<u8>,
}

impl Event {
    pub fn new(kind: &str, data: Value) -> Self {
        Self {
            kind: kind.to_owned(),
            data: match data {
                Value::Object(map) => map,
                _ => Map::new(),
            },
            payload: Vec::new(),
        }
    }
    fn number(&self, key: &str) -> Option<u64> {
        self.data.get(key).and_then(Value::as_u64)
    }
    fn text(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Value::as_str)
    }
    fn format(&self) -> Option<audio::Format> {
        let format = audio::Format {
            rate: u32::try_from(self.number("rate")?).ok()?,
            width: u16::try_from(self.number("width")?).ok()?,
            channels: u16::try_from(self.number("channels")?).ok()?,
        };
        format.valid().then_some(format)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    Closed,
    Malformed,
    Limit,
    Io,
}

impl FrameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Malformed => "malformed_event",
            Self::Limit => "event_limit",
            Self::Io => "io_error",
        }
    }
}

async fn exact<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    length: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::Closed
        } else {
            FrameError::Io
        }
    })?;
    Ok(bytes)
}

/// Bytes after the header line are merged over inline `data`, as the reference reader does.
pub async fn read_event<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Event, FrameError> {
    let mut line = Vec::new();
    let read = (&mut *reader)
        .take(MAX_HEADER_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|_| FrameError::Io)?;
    if read == 0 {
        return Err(FrameError::Closed);
    }
    if line.last() != Some(&b'\n') {
        return Err(if line.len() > MAX_HEADER_BYTES {
            FrameError::Limit
        } else {
            FrameError::Closed
        });
    }
    let header: Map<String, Value> =
        serde_json::from_slice(&line).map_err(|_| FrameError::Malformed)?;
    let kind = header
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty() && kind.len() <= 128)
        .ok_or(FrameError::Malformed)?
        .to_owned();
    let length = |key: &str, limit: usize| match header.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => match value.as_u64() {
            Some(length) if length as usize <= limit => Ok(length as usize),
            Some(_) => Err(FrameError::Limit),
            None => Err(FrameError::Malformed),
        },
    };
    let data_length = length("data_length", MAX_DATA_BYTES)?;
    let payload_length = length("payload_length", MAX_PAYLOAD_BYTES)?;
    let mut data = match header.get("data") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(FrameError::Malformed),
    };
    if data_length > 0 {
        let extra: Map<String, Value> = serde_json::from_slice(&exact(reader, data_length).await?)
            .map_err(|_| FrameError::Malformed)?;
        data.extend(extra);
    }
    let payload = if payload_length > 0 {
        exact(reader, payload_length).await?
    } else {
        Vec::new()
    };
    Ok(Event {
        kind,
        data,
        payload,
    })
}

pub fn encode_event(event: &Event) -> Vec<u8> {
    let data = (!event.data.is_empty())
        .then(|| serde_json::to_vec(&event.data).expect("event data serializes"));
    let mut header = json!({"type": event.kind, "version": PROTOCOL_VERSION});
    if let Some(data) = &data {
        header["data_length"] = json!(data.len());
    }
    if !event.payload.is_empty() {
        header["payload_length"] = json!(event.payload.len());
    }
    let mut bytes = serde_json::to_vec(&header).expect("event header serializes");
    bytes.push(b'\n');
    if let Some(data) = data {
        bytes.extend(data);
    }
    bytes.extend_from_slice(&event.payload);
    bytes
}

pub async fn write_event<W: AsyncWrite + Unpin>(
    writer: &mut W,
    event: &Event,
) -> std::io::Result<()> {
    writer.write_all(&encode_event(event)).await
}

/// Native Juiced v0.9.2 served Wyoming through wyoming_openai 0.8.0; Home Assistant keeps
/// its entities and pipeline selections only while this identity stays the same.
const PROXY_VERSION: &str = "0.8.0";
const PROXY_PROGRAM: &str = "openai";
const PROXY_DESCRIPTION: &str = "OpenAI (Non-Streaming)";
const PROXY_URL: &str = "https://github.com/roryeckel/wyoming_openai";
const ATTRIBUTION_PROGRAM: &str = "OpenAI-Compatible Proxy";
const ATTRIBUTION_MODEL: &str = "OpenAI-Compatible Wyoming Proxy";
const ADVERTISED_LANGUAGES: &[&str] = &["en"];
const ADVERTISED_VOICE_PREFIXES: &[&str] = &["af_", "am_", "bf_", "bm_"];
static HTTP_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

pub fn set_http_port(port: u16) {
    let _ = HTTP_PORT.set(port);
}

fn openai_url() -> String {
    format!(
        "http://127.0.0.1:{}/v1",
        HTTP_PORT.get().copied().unwrap_or(8080)
    )
}

fn attribution(name: &str, url: &str) -> Value {
    json!({"name": name, "url": url})
}

pub fn advertised_voices(resolved: &Resolved, pinned: Option<&str>) -> Vec<String> {
    let mut voices: Vec<String> = resolved
        .tts_voices
        .iter()
        .filter(|voice| {
            Some(voice.as_str()) == pinned
                || ADVERTISED_VOICE_PREFIXES
                    .iter()
                    .any(|prefix| voice.starts_with(prefix))
        })
        .cloned()
        .collect();
    voices.sort();
    voices.dedup();
    if voices.is_empty() {
        voices.extend(resolved.tts_voices.first().cloned());
    }
    voices
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Resolved {
    pub stt_models: Vec<String>,
    pub tts_model: Option<String>,
    pub tts_voices: Vec<String>,
}

pub async fn resolve(manager: &Manager, settings: &Settings) -> Resolved {
    let stt_models = match &settings.stt_model {
        Some(model) => vec![model.clone()],
        None => crate::voice::models(manager, Operation::Transcription).await,
    };
    let tts_model = match &settings.tts_model {
        Some(model) => Some(model.clone()),
        None => crate::voice::models(manager, Operation::Speech)
            .await
            .into_iter()
            .next(),
    };
    let mut tts_voices = Vec::new();
    if let Some(model) = &tts_model {
        let listed = crate::voice::listed_voices(manager, model).await;
        let default = settings
            .tts_voice
            .clone()
            .unwrap_or_else(|| crate::voice::default_voice(&listed));
        tts_voices.push(default);
        for voice in listed {
            if !tts_voices.contains(&voice) {
                tts_voices.push(voice);
            }
        }
    }
    Resolved {
        stt_models,
        tts_model,
        tts_voices,
    }
}

pub fn info(resolved: &Resolved, pinned_voice: Option<&str>) -> Event {
    let url = openai_url();
    let asr: Vec<Value> = if resolved.stt_models.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "name": PROXY_PROGRAM,
            "attribution": attribution(ATTRIBUTION_PROGRAM, &url),
            "installed": true,
            "description": PROXY_DESCRIPTION,
            "version": PROXY_VERSION,
            "models": resolved.stt_models.iter().map(|model| json!({
                "name": model,
                "attribution": attribution(ATTRIBUTION_MODEL, &url),
                "installed": true,
                "description": model,
                "version": null,
                "languages": ADVERTISED_LANGUAGES,
            })).collect::<Vec<_>>(),
            "supports_transcript_streaming": false,
            "requires_external_vad": true,
            "prefers_auto_gain_enabled": true,
            "prefers_noise_reduction_enabled": true,
        })]
    };
    let tts: Vec<Value> = match &resolved.tts_model {
        None => Vec::new(),
        Some(_) => vec![json!({
            "name": PROXY_PROGRAM,
            "attribution": attribution(ATTRIBUTION_PROGRAM, PROXY_URL),
            "installed": true,
            "description": PROXY_DESCRIPTION,
            "version": PROXY_VERSION,
            "voices": advertised_voices(resolved, pinned_voice).iter().map(|voice| json!({
                "name": voice,
                "attribution": attribution(ATTRIBUTION_MODEL, &url),
                "installed": true,
                "description": voice,
                "version": null,
                "languages": ADVERTISED_LANGUAGES,
                "speakers": null,
            })).collect::<Vec<_>>(),
            "supports_synthesize_streaming": false,
        })],
    };
    Event::new(
        "info",
        json!({"asr": asr, "tts": tts, "handle": [], "intent": [], "wake": [], "mic": [], "snd": []}),
    )
}

#[derive(Clone, Debug, Serialize)]
pub struct ListenerError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventSummary {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub model: Option<String>,
    pub outcome: &'static str,
    pub duration_ms: u64,
    pub audio_ms: u64,
    pub at_unix_ms: u64,
}

#[derive(Default)]
struct Status {
    listening: bool,
    address: Option<String>,
    error: Option<ListenerError>,
    last_event: Option<EventSummary>,
}

struct Shared {
    manager: Arc<Manager>,
    status: Mutex<Status>,
    active: AtomicUsize,
    accepted: AtomicU64,
}

impl Shared {
    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    fn record(&self, summary: EventSummary) {
        self.status().last_event = Some(summary);
    }
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub struct Controller {
    shared: Arc<Shared>,
    running: AsyncMutex<Option<Running>>,
}

impl Controller {
    pub fn new(manager: Arc<Manager>) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(Shared {
                manager,
                status: Mutex::new(Status::default()),
                active: AtomicUsize::new(0),
                accepted: AtomicU64::new(0),
            }),
            running: AsyncMutex::new(None),
        })
    }

    pub async fn start_configured(&self) {
        let mut running = self.running.lock().await;
        let (settings, _) = self.shared.manager.wyoming_settings().await;
        self.apply(&mut running, &settings).await;
    }

    async fn apply(&self, running: &mut Option<Running>, settings: &Settings) {
        if let Some(previous) = running.take() {
            let _ = previous.stop.send(true);
            let _ = previous.task.await;
        }
        {
            let mut status = self.shared.status();
            status.listening = false;
            status.address = None;
            status.error = None;
        }
        if !settings.enabled {
            return;
        }
        let listener = match settings.listen.parse::<SocketAddr>() {
            Ok(address) => TcpListener::bind(address)
                .await
                .map_err(|error| error.to_string()),
            Err(_) => Err("invalid listen address".to_owned()),
        };
        let listener = match listener {
            Ok(listener) => listener,
            Err(error) => {
                self.shared.manager.logs().service(
                    Level::Error,
                    &format!(
                        "Wyoming listener could not bind {}: {error}",
                        settings.listen
                    ),
                );
                self.shared.status().error = Some(ListenerError {
                    code: "wyoming_bind_failed",
                    message: format!("Could not listen on {}: {error}", settings.listen),
                });
                return;
            }
        };
        let address = listener
            .local_addr()
            .map(|address| address.to_string())
            .unwrap_or_else(|_| settings.listen.clone());
        self.shared.manager.logs().service(
            Level::Info,
            &format!("Wyoming listening on tcp://{address}"),
        );
        {
            let mut status = self.shared.status();
            status.listening = true;
            status.address = Some(address);
        }
        let (stop, stopped) = watch::channel(false);
        let shared = self.shared.clone();
        let settings = Arc::new(settings.clone());
        let task = tokio::spawn(accept(shared, listener, settings, stopped));
        *running = Some(Running { stop, task });
    }

    pub async fn view(&self) -> Value {
        let (settings, generation) = self.shared.manager.wyoming_settings().await;
        self.render(settings, generation).await
    }

    async fn render(&self, settings: Settings, generation: u64) -> Value {
        let resolved = resolve(&self.shared.manager, &settings).await;
        let status = self.shared.status();
        json!({
            "schema_version": 1,
            "configuration_generation": generation,
            "settings": settings,
            "listener": {
                "listening": status.listening,
                "address": status.address,
                "error": status.error,
            },
            "active_connections": self.shared.active.load(Ordering::Acquire),
            "total_connections": self.shared.accepted.load(Ordering::Acquire),
            "last_event": status.last_event,
            "resolved": resolved,
        })
    }

    /// The restart lock spans persistence and rebinding so concurrent changes cannot leave
    /// the listener on an older generation than the saved configuration.
    pub async fn configure(&self, change: Change) -> Result<Value, ApiError> {
        change.settings.validate().map_err(|message| {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_wyoming_settings", message)
        })?;
        let mut running = self.running.lock().await;
        let generation = self
            .shared
            .manager
            .configure_wyoming(change.settings.clone(), change.expected_generation)
            .await?;
        let (effective, _) = self.shared.manager.wyoming_settings().await;
        self.apply(&mut running, &effective).await;
        drop(running);
        Ok(self.render(effective, generation).await)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub settings: Settings,
    pub expected_generation: u64,
}

struct ConnectionSlot(Arc<Shared>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn accept(
    shared: Arc<Shared>,
    listener: TcpListener,
    settings: Arc<Settings>,
    mut stopped: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stopped.changed() => return,
        };
        let Ok((stream, _)) = accepted else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        if shared.active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            shared.active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        shared.accepted.fetch_add(1, Ordering::AcqRel);
        let slot = ConnectionSlot(shared.clone());
        let settings = settings.clone();
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            connection(slot, stream, settings).await;
        });
    }
}

struct Stream {
    format: audio::Format,
    samples: Vec<i16>,
    overflow: bool,
}

struct Request {
    model: Option<String>,
    language: Option<String>,
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn error_event(code: &str, text: &str) -> Event {
    Event::new("error", json!({"text": text, "code": code}))
}

async fn connection(slot: ConnectionSlot, stream: TcpStream, settings: Arc<Settings>) {
    let shared = slot.0.clone();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut request: Option<Request> = None;
    let mut audio: Option<Stream> = None;
    loop {
        let event = match tokio::time::timeout(IDLE_TIMEOUT, read_event(&mut reader)).await {
            Err(_) | Ok(Err(FrameError::Closed | FrameError::Io)) => return,
            Ok(Err(error)) => {
                let _ = write_event(
                    &mut write,
                    &error_event(error.code(), "The event could not be read."),
                )
                .await;
                return;
            }
            Ok(Ok(event)) => event,
        };
        let replies = match event.kind.as_str() {
            "describe" => vec![info(
                &resolve(&shared.manager, &settings).await,
                settings.tts_voice.as_deref(),
            )],
            "ping" => vec![Event::new("pong", json!({"text": event.text("text")}))],
            "transcribe" => {
                request = Some(Request {
                    model: event.text("name").map(str::to_owned),
                    language: event.text("language").map(str::to_owned),
                });
                Vec::new()
            }
            "audio-start" => match event.format() {
                Some(format) => {
                    audio = Some(Stream {
                        format,
                        samples: Vec::new(),
                        overflow: false,
                    });
                    Vec::new()
                }
                None => vec![error_event(
                    "invalid_audio_format",
                    "audio-start needs a supported rate, width and channels.",
                )],
            },
            "audio-chunk" => append(&mut audio, &event),
            "audio-stop" => match audio.take() {
                Some(stream) if !stream.overflow => {
                    vec![transcribe(&shared, &settings, request.take(), stream).await]
                }
                Some(_) => {
                    request = None;
                    Vec::new()
                }
                None => vec![error_event(
                    "audio_not_started",
                    "Send audio-start before audio-stop.",
                )],
            },
            "synthesize" => synthesize(&shared, &settings, &event).await,
            _ => Vec::new(),
        };
        for reply in replies {
            if write_event(&mut write, &reply).await.is_err() {
                return;
            }
        }
    }
}

fn append(audio: &mut Option<Stream>, event: &Event) -> Vec<Event> {
    let format = event.format();
    let stream = audio.get_or_insert_with(|| Stream {
        format: format.unwrap_or(audio::Format {
            rate: audio::WHISPER_RATE,
            width: 2,
            channels: 1,
        }),
        samples: Vec::new(),
        overflow: false,
    });
    if stream.overflow {
        return Vec::new();
    }
    let chunk_format = format.unwrap_or(stream.format);
    let mut samples = audio::mono_i16(&event.payload, chunk_format);
    if chunk_format.rate != stream.format.rate {
        samples = audio::resample(&samples, chunk_format.rate, stream.format.rate);
    }
    let whisper_samples = (stream.samples.len() + samples.len()) as u64
        * u64::from(audio::WHISPER_RATE)
        / u64::from(stream.format.rate);
    if stream.samples.len() + samples.len() > crate::voice::MAX_TRANSCRIPTION_SAMPLES
        || whisper_samples > crate::voice::MAX_TRANSCRIPTION_SAMPLES as u64
    {
        stream.overflow = true;
        stream.samples = Vec::new();
        return vec![error_event(
            "audio_limit",
            "The audio stream exceeded its bounded duration.",
        )];
    }
    stream.samples.extend(samples);
    Vec::new()
}

async fn transcribe(
    shared: &Shared,
    settings: &Settings,
    request: Option<Request>,
    stream: Stream,
) -> Event {
    let started = Instant::now();
    let request = request.unwrap_or(Request {
        model: None,
        language: None,
    });
    let resolved = resolve_stt(&shared.manager, settings, request.model.as_deref()).await;
    let audio_ms = stream.samples.len() as u64 * 1000 / u64::from(stream.format.rate);
    let samples = audio::resample(&stream.samples, stream.format.rate, audio::WHISPER_RATE);
    let result = match &resolved {
        Some(model) => {
            crate::voice::transcribe(
                &shared.manager,
                model,
                &samples,
                crate::voice::language(request.language.as_deref()),
            )
            .await
        }
        None => Err(no_model("transcription")),
    };
    shared.record(EventSummary {
        kind: "transcribe",
        model: resolved,
        outcome: result
            .as_ref()
            .map_or_else(|error| error.code, |_| "completed"),
        duration_ms: started.elapsed().as_millis() as u64,
        audio_ms,
        at_unix_ms: unix_ms(),
    });
    match result {
        Ok(text) => Event::new("transcript", json!({"text": text})),
        Err(error) => error_event(error.code, &error.message),
    }
}

fn no_model(operation: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "wyoming_model_unavailable",
        format!(
            "No enabled {operation} model is available. Enable one or set it in the Wyoming settings."
        ),
    )
}

async fn resolve_stt(
    manager: &Manager,
    settings: &Settings,
    requested: Option<&str>,
) -> Option<String> {
    let models = match &settings.stt_model {
        Some(model) => vec![model.clone()],
        None => crate::voice::models(manager, Operation::Transcription).await,
    };
    requested
        .and_then(|name| models.iter().find(|model| *model == name).cloned())
        .or_else(|| models.into_iter().next())
}

async fn synthesize(shared: &Shared, settings: &Settings, event: &Event) -> Vec<Event> {
    let started = Instant::now();
    let Some(text) = event.text("text") else {
        return vec![error_event(
            "invalid_speech_request",
            "synthesize needs text.",
        )];
    };
    let model = match &settings.tts_model {
        Some(model) => Some(model.clone()),
        None => crate::voice::models(&shared.manager, Operation::Speech)
            .await
            .into_iter()
            .next(),
    };
    let requested = event
        .data
        .get("voice")
        .and_then(|voice| voice.get("name"))
        .and_then(Value::as_str)
        .filter(|voice| crate::media::valid_voice_selection(voice));
    let result = match &model {
        None => Err(no_model("speech")),
        Some(model) => {
            let voice = match requested
                .map(str::to_owned)
                .or_else(|| settings.tts_voice.clone())
            {
                Some(voice) => voice,
                None => crate::voice::default_voice(
                    &crate::voice::listed_voices(&shared.manager, model).await,
                ),
            };
            crate::voice::synthesize(&shared.manager, model, text, &voice).await
        }
    };
    let audio_ms = result.as_ref().map_or(0, |decoded| {
        decoded.samples.len() as u64 * 1000 / u64::from(decoded.rate.max(1))
    });
    shared.record(EventSummary {
        kind: "synthesize",
        model,
        outcome: result
            .as_ref()
            .map_or_else(|error| error.code, |_| "completed"),
        duration_ms: started.elapsed().as_millis() as u64,
        audio_ms,
        at_unix_ms: unix_ms(),
    });
    match result {
        Ok(decoded) => speech_events(&decoded),
        Err(error) => vec![error_event(error.code, &error.message)],
    }
}

pub fn speech_events(decoded: &audio::Decoded) -> Vec<Event> {
    let format = json!({"rate": decoded.rate, "width": 2, "channels": 1});
    let mut events = Vec::with_capacity(decoded.samples.len() / SAMPLES_PER_CHUNK + 2);
    let mut start = format.clone();
    start["timestamp"] = json!(0);
    events.push(Event::new("audio-start", start));
    let mut elapsed = 0u64;
    for chunk in decoded.samples.chunks(SAMPLES_PER_CHUNK) {
        let mut data = format.clone();
        data["timestamp"] = json!(elapsed * 1000 / u64::from(decoded.rate.max(1)));
        events.push(Event {
            kind: "audio-chunk".to_owned(),
            data: data.as_object().cloned().unwrap_or_default(),
            payload: audio::to_le_bytes(chunk),
        });
        elapsed += chunk.len() as u64;
    }
    events.push(Event::new(
        "audio-stop",
        json!({"timestamp": elapsed * 1000 / u64::from(decoded.rate.max(1))}),
    ));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn decode(bytes: &[u8]) -> Result<Event, FrameError> {
        let mut reader = BufReader::new(bytes);
        read_event(&mut reader).await
    }

    #[tokio::test]
    async fn frames_round_trip_with_separate_data_and_payload() {
        let event = Event {
            kind: "audio-chunk".into(),
            data: json!({"rate": 16000, "width": 2, "channels": 1})
                .as_object()
                .unwrap()
                .clone(),
            payload: vec![1, 2, 3, 4],
        };
        let bytes = encode_event(&event);
        let newline = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        let header: Value = serde_json::from_slice(&bytes[..newline]).unwrap();
        assert_eq!(header["type"], "audio-chunk");
        assert_eq!(header["version"], PROTOCOL_VERSION);
        assert_eq!(header["payload_length"], 4);
        assert!(header.get("data").is_none());
        assert_eq!(decode(&bytes).await.unwrap(), event);
        let bare = encode_event(&Event::new("describe", Value::Null));
        assert_eq!(
            bare,
            format!("{{\"type\":\"describe\",\"version\":\"{PROTOCOL_VERSION}\"}}\n").as_bytes()
        );
    }

    #[tokio::test]
    async fn trailing_data_overrides_inline_data_and_sequences_stay_aligned() {
        let mut bytes =
            br#"{"type":"transcribe","data":{"language":"de","name":"a"},"data_length":17}"#
                .to_vec();
        bytes.push(b'\n');
        bytes.extend(br#"{"language":"en"}"#);
        bytes.extend(encode_event(&Event::new(
            "audio-stop",
            json!({"timestamp": 5}),
        )));
        let mut reader = BufReader::new(bytes.as_slice());
        let first = read_event(&mut reader).await.unwrap();
        assert_eq!(first.text("language"), Some("en"));
        assert_eq!(first.text("name"), Some("a"));
        let second = read_event(&mut reader).await.unwrap();
        assert_eq!(second.kind, "audio-stop");
        assert_eq!(read_event(&mut reader).await, Err(FrameError::Closed));
    }

    #[tokio::test]
    async fn malformed_oversized_and_truncated_frames_fail_closed() {
        assert_eq!(decode(b"not json\n").await, Err(FrameError::Malformed));
        assert_eq!(decode(b"{\"data\":{}}\n").await, Err(FrameError::Malformed));
        assert_eq!(
            decode(b"{\"type\":\"x\",\"data\":[]}\n").await,
            Err(FrameError::Malformed)
        );
        assert_eq!(
            decode(
                format!(
                    "{{\"type\":\"x\",\"payload_length\":{}}}\n",
                    MAX_PAYLOAD_BYTES + 1
                )
                .as_bytes()
            )
            .await,
            Err(FrameError::Limit)
        );
        assert_eq!(
            decode(b"{\"type\":\"x\",\"payload_length\":-1}\n").await,
            Err(FrameError::Malformed)
        );
        assert_eq!(
            decode(b"{\"type\":\"x\",\"payload_length\":8}\nabc").await,
            Err(FrameError::Closed)
        );
        let long = vec![b' '; MAX_HEADER_BYTES + 10];
        assert_eq!(decode(&long).await, Err(FrameError::Limit));
        assert_eq!(decode(b"{\"type\":\"x\"}").await, Err(FrameError::Closed));
    }

    #[test]
    fn info_is_identical_to_wyoming_openai_for_the_native_configuration() {
        let fixture: Value =
            serde_json::from_str(include_str!("../fixtures/wyoming_openai-0.8.0-info.json"))
                .unwrap();
        assert_eq!(fixture["_capture"]["header"]["version"], PROTOCOL_VERSION);
        let mut listed: Vec<String> = [
            "af_heart",
            "af_alloy",
            "zf_xiaobei",
            "alloy",
            "bm_george",
            "ef_dora",
            "am_adam",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        listed.extend(
            fixture["data"]["tts"][0]["voices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|voice| voice["name"].as_str().unwrap().to_owned()),
        );
        let resolved = Resolved {
            stt_models: vec!["asr4all-l-gptq-Q8_0".into()],
            tts_model: Some("kokoro-v1".into()),
            tts_voices: listed,
        };
        let info = info(&resolved, None);
        assert_eq!(info.kind, "info");
        assert_eq!(Value::Object(info.data), fixture["data"]);
        let empty = super::info(&Resolved::default(), None);
        assert_eq!(empty.data["asr"], json!([]));
        assert_eq!(empty.data["tts"], json!([]));
    }

    #[test]
    fn pinned_voices_stay_advertised_and_an_empty_pack_keeps_the_default() {
        let resolved = Resolved {
            stt_models: Vec::new(),
            tts_model: Some("kokoro".into()),
            tts_voices: vec!["ef_dora".into(), "bm_george".into(), "jf_alpha".into()],
        };
        assert_eq!(
            advertised_voices(&resolved, Some("ef_dora")),
            ["bm_george", "ef_dora"]
        );
        let only_default = Resolved {
            tts_voices: vec!["af_heart".into()],
            ..resolved
        };
        assert_eq!(advertised_voices(&only_default, None), ["af_heart"]);
    }

    #[test]
    fn native_environment_enables_the_default_listener() {
        assert_eq!(Settings::from_environment(None, None), None);
        assert_eq!(Settings::from_environment(Some("false"), None), None);
        let enabled = Settings::from_environment(Some("true"), None)
            .unwrap()
            .unwrap();
        assert!(enabled.enabled);
        assert_eq!(enabled.listen, DEFAULT_LISTEN);
        assert_eq!(enabled.stt_model, None);
        assert_eq!(enabled.tts_model, None);
        let custom = Settings::from_environment(Some("true"), Some("tcp://127.0.0.1:10301"))
            .unwrap()
            .unwrap();
        assert_eq!(custom.listen, "127.0.0.1:10301");
        assert!(
            Settings::from_environment(Some("true"), Some("unix:///run/w.sock"))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn settings_validate_listen_models_and_voice() {
        assert!(Settings::default().validate().is_ok());
        assert!(Settings::default().is_default());
        let bad = |edit: fn(&mut Settings)| {
            let mut settings = Settings::default();
            edit(&mut settings);
            settings.validate().is_err()
        };
        assert!(bad(|settings| settings.listen = "localhost:10300".into()));
        assert!(bad(|settings| settings.stt_model = Some(String::new())));
        assert!(bad(|settings| settings.tts_model = Some("a\nb".into())));
        assert!(bad(|settings| settings.tts_voice = Some("../x".into())));
        let parsed: Settings = serde_json::from_value(json!({"enabled": true})).unwrap();
        assert_eq!(parsed.listen, DEFAULT_LISTEN);
        assert!(serde_json::from_value::<Settings>(json!({"port": 1})).is_err());
    }

    #[test]
    fn chunks_carry_format_and_resampled_streams_stay_bounded() {
        let decoded = audio::Decoded {
            rate: 22_050,
            samples: vec![7; SAMPLES_PER_CHUNK * 2 + 1],
        };
        let events = speech_events(&decoded);
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].kind, "audio-start");
        assert_eq!(events[0].data["rate"], 22_050);
        assert_eq!(events[1].data["width"], 2);
        assert_eq!(events[3].payload.len(), 2);
        assert_eq!(events[4].kind, "audio-stop");
        let mut stream = None;
        let chunk = Event {
            kind: "audio-chunk".into(),
            data: json!({"rate": 48000, "width": 2, "channels": 2})
                .as_object()
                .unwrap()
                .clone(),
            payload: vec![0; 48_000 * 4],
        };
        assert!(append(&mut stream, &chunk).is_empty());
        assert_eq!(stream.as_ref().unwrap().samples.len(), 48_000);
        let mut errors = Vec::new();
        for _ in 0..200 {
            errors.extend(append(&mut stream, &chunk));
        }
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].text("code"), Some("audio_limit"));
        assert!(stream.unwrap().overflow);
    }
}
