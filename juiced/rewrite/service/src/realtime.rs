//! OpenAI-compatible Realtime transcription sessions over WebSocket.

use crate::{ApiError, audio, config::Operation, supervisor::Manager};
use axum::{
    extract::ws::{Message, WebSocket},
    http::StatusCode,
};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

pub const INPUT_RATE: u32 = 24_000;
pub const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SESSIONS: usize = 16;
const MAX_BUFFER_SAMPLES: usize = crate::voice::MAX_TRANSCRIPTION_SAMPLES;
const MIN_COMMIT_SAMPLES: usize = INPUT_RATE as usize / 10;
const QUEUED_TRANSCRIPTIONS: usize = 2;
const MAX_TEXT_BYTES: usize = 4096;

#[derive(Deserialize)]
pub struct Query {
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Default)]
pub struct Sessions {
    active: AtomicUsize,
    sequence: AtomicU64,
}

pub struct SessionSlot(Arc<Sessions>);
impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Sessions {
    pub fn admit(self: &Arc<Self>, query: &Query) -> Result<SessionSlot, ApiError> {
        if query
            .intent
            .as_deref()
            .is_some_and(|intent| intent != "transcription")
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_realtime_intent",
                "Only intent=transcription Realtime sessions are supported.",
            ));
        }
        if query
            .model
            .as_deref()
            .is_some_and(|model| !valid_text(model, 256))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_realtime_query",
                "model must be a bounded registered model ID.",
            ));
        }
        if self.active.fetch_add(1, Ordering::AcqRel) >= MAX_SESSIONS {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "realtime_sessions_exhausted",
                "Too many Realtime sessions are open. Close one and retry.",
            ));
        }
        Ok(SessionSlot(self.clone()))
    }
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq)]
struct Session {
    ga: bool,
    model: Option<String>,
    language: Option<String>,
    prompt: Option<String>,
    vad: Option<audio::VadSettings>,
}

const DEFAULT_VAD: audio::VadSettings = audio::VadSettings {
    threshold: 0.5,
    silence_ms: 500,
    prefix_padding_ms: 300,
};

struct Invalid {
    code: &'static str,
    message: &'static str,
    param: &'static str,
}

fn invalid(code: &'static str, param: &'static str, message: &'static str) -> Invalid {
    Invalid {
        code,
        message,
        param,
    }
}

impl Session {
    fn view(&self, id: &str) -> Value {
        let turn_detection = self.vad.map(|vad| {
            json!({"type": "server_vad", "threshold": vad.threshold, "prefix_padding_ms": vad.prefix_padding_ms, "silence_duration_ms": vad.silence_ms})
        });
        let transcription =
            json!({"model": self.model, "language": self.language, "prompt": self.prompt});
        if self.ga {
            json!({
                "id": id, "object": "realtime.transcription_session", "type": "transcription",
                "audio": {"input": {
                    "format": {"type": "audio/pcm", "rate": INPUT_RATE},
                    "transcription": transcription,
                    "turn_detection": turn_detection,
                    "noise_reduction": null,
                }},
                "include": null,
            })
        } else {
            json!({
                "id": id, "object": "realtime.transcription_session",
                "input_audio_format": "pcm16",
                "input_audio_transcription": transcription,
                "turn_detection": turn_detection,
                "input_audio_noise_reduction": null,
                "include": null,
            })
        }
    }

    /// Accepts the beta flat shape and the GA `audio.input` shape; unknown fields are left
    /// to the client because OpenAI SDKs send options this gateway has no use for.
    fn update(&mut self, session: &Value) -> Result<(), Invalid> {
        let Some(session) = session.as_object() else {
            return Err(invalid(
                "invalid_session",
                "session",
                "session must be an object.",
            ));
        };
        let input = session.get("audio").and_then(|audio| audio.get("input"));
        let field = |flat: &str, nested: &str| {
            session
                .get(flat)
                .or_else(|| input.and_then(|input| input.get(nested)))
        };
        match field("input_audio_format", "format") {
            None => {}
            Some(Value::String(format)) if format == "pcm16" => {}
            Some(Value::Object(format))
                if format.get("type").and_then(Value::as_str) == Some("audio/pcm")
                    && format
                        .get("rate")
                        .is_none_or(|rate| rate.as_u64() == Some(u64::from(INPUT_RATE))) => {}
            Some(_) => {
                return Err(invalid(
                    "unsupported_audio_format",
                    "input_audio_format",
                    "Only 24 kHz mono pcm16 input audio is supported.",
                ));
            }
        }
        let mut next = self.clone();
        match field("input_audio_transcription", "transcription") {
            None => {}
            Some(Value::Null) => {
                next.model = None;
                next.language = None;
                next.prompt = None;
            }
            Some(Value::Object(transcription)) => {
                let text = |key: &str, limit: usize| match transcription.get(key) {
                    None => Ok(None),
                    Some(Value::Null) => Ok(Some(None)),
                    Some(Value::String(value)) if value.is_empty() => Ok(Some(None)),
                    Some(Value::String(value)) if valid_text(value, limit) => {
                        Ok(Some(Some(value.clone())))
                    }
                    Some(_) => Err(()),
                };
                let bad = |param| {
                    invalid(
                        "invalid_transcription",
                        param,
                        "Transcription fields must be bounded strings.",
                    )
                };
                if let Some(model) =
                    text("model", 256).map_err(|_| bad("input_audio_transcription.model"))?
                {
                    next.model = model;
                }
                if let Some(language) =
                    text("language", 16).map_err(|_| bad("input_audio_transcription.language"))?
                {
                    next.language = language;
                }
                if let Some(prompt) = text("prompt", MAX_TEXT_BYTES)
                    .map_err(|_| bad("input_audio_transcription.prompt"))?
                {
                    next.prompt = prompt;
                }
            }
            Some(_) => {
                return Err(invalid(
                    "invalid_transcription",
                    "input_audio_transcription",
                    "input_audio_transcription must be an object or null.",
                ));
            }
        }
        match field("turn_detection", "turn_detection") {
            None => {}
            Some(Value::Null) => next.vad = None,
            Some(Value::Object(detection)) => {
                if detection
                    .get("type")
                    .is_some_and(|kind| kind.as_str() != Some("server_vad"))
                {
                    return Err(invalid(
                        "unsupported_turn_detection",
                        "turn_detection.type",
                        "Only server_vad turn detection is supported.",
                    ));
                }
                let mut vad = next.vad.unwrap_or(DEFAULT_VAD);
                let bad = |param| {
                    invalid(
                        "invalid_turn_detection",
                        param,
                        "turn_detection values are out of range.",
                    )
                };
                if let Some(value) = detection.get("threshold") {
                    vad.threshold = value
                        .as_f64()
                        .filter(|value| (0.0..=1.0).contains(value))
                        .ok_or_else(|| bad("turn_detection.threshold"))?;
                }
                if let Some(value) = detection.get("silence_duration_ms") {
                    vad.silence_ms = value
                        .as_u64()
                        .filter(|value| (20..=10_000).contains(value))
                        .ok_or_else(|| bad("turn_detection.silence_duration_ms"))?
                        as u32;
                }
                if let Some(value) = detection.get("prefix_padding_ms") {
                    vad.prefix_padding_ms = value
                        .as_u64()
                        .filter(|value| *value <= 5_000)
                        .ok_or_else(|| bad("turn_detection.prefix_padding_ms"))?
                        as u32;
                }
                next.vad = Some(vad);
            }
            Some(_) => {
                return Err(invalid(
                    "invalid_turn_detection",
                    "turn_detection",
                    "turn_detection must be an object or null.",
                ));
            }
        }
        *self = next;
        Ok(())
    }
}

struct Job {
    item: String,
    samples: Vec<i16>,
    model: Option<String>,
    language: Option<String>,
}

struct Connection {
    id: String,
    events: AtomicU64,
    items: AtomicU64,
    out: mpsc::Sender<Value>,
}

impl Connection {
    async fn send(&self, kind: &str, mut event: Value) {
        event["type"] = json!(kind);
        event["event_id"] = json!(format!(
            "event_{}_{}",
            self.id,
            self.events.fetch_add(1, Ordering::AcqRel)
        ));
        let _ = self.out.send(event).await;
    }
    fn item(&self) -> String {
        format!(
            "item_{}_{}",
            self.id,
            self.items.fetch_add(1, Ordering::AcqRel)
        )
    }
    async fn error(
        &self,
        code: &str,
        param: Option<&str>,
        message: &str,
        client_event: Option<&str>,
    ) {
        self.send(
            "error",
            json!({"error": {"type": "invalid_request_error", "code": code, "message": message, "param": param, "event_id": client_event}}),
        )
        .await;
    }
}

struct Buffer {
    samples: Vec<i16>,
    origin_ms: u64,
    vad_origin_ms: u64,
    vad: Option<audio::Vad>,
    speech_start_ms: Option<u64>,
    item: Option<String>,
    previous: Option<String>,
}

impl Buffer {
    fn new(vad: Option<audio::VadSettings>, origin_ms: u64) -> Self {
        Self {
            samples: Vec::new(),
            origin_ms,
            vad_origin_ms: origin_ms,
            vad: vad.map(|settings| audio::Vad::new(settings, INPUT_RATE)),
            speech_start_ms: None,
            item: None,
            previous: None,
        }
    }
    fn end_ms(&self) -> u64 {
        self.origin_ms + self.samples.len() as u64 * 1000 / u64::from(INPUT_RATE)
    }
    fn reset(&mut self, vad: Option<audio::VadSettings>) {
        let previous = self.previous.take();
        *self = Self::new(vad, self.end_ms());
        self.previous = previous;
    }
    fn take(&mut self, from_ms: u64) -> Vec<i16> {
        let skip = (from_ms.saturating_sub(self.origin_ms) * u64::from(INPUT_RATE) / 1000) as usize;
        let end = self.end_ms();
        let samples = std::mem::take(&mut self.samples);
        self.origin_ms = end;
        self.speech_start_ms = None;
        samples.into_iter().skip(skip).collect()
    }
    /// Without speech in progress only the prefix padding window can still belong to a turn.
    fn trim_idle(&mut self, prefix_ms: u32) {
        let keep = (u64::from(prefix_ms) * u64::from(INPUT_RATE) / 1000) as usize
            + INPUT_RATE as usize / 10;
        if self.speech_start_ms.is_none() && self.samples.len() > keep * 2 {
            let drop = self.samples.len() - keep;
            self.samples.drain(..drop);
            self.origin_ms += drop as u64 * 1000 / u64::from(INPUT_RATE);
        }
    }
}

pub async fn serve(manager: Arc<Manager>, socket: WebSocket, query: Query, slot: SessionSlot) {
    let id = format!(
        "{:x}",
        slot.0.sequence.fetch_add(1, Ordering::AcqRel) ^ random_tag()
    );
    let (mut sink, mut stream) = socket.split();
    let (out, mut outgoing) = mpsc::channel::<Value>(64);
    let writer = tokio::spawn(async move {
        while let Some(event) = outgoing.recv().await {
            if sink
                .send(Message::Text(event.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let connection = Arc::new(Connection {
        id: id.clone(),
        events: AtomicU64::new(0),
        items: AtomicU64::new(0),
        out,
    });
    let closed = Arc::new(AtomicBool::new(false));
    let (jobs, queue) = mpsc::channel::<Job>(QUEUED_TRANSCRIPTIONS);
    let worker = tokio::spawn(transcriber(
        manager.clone(),
        connection.clone(),
        closed.clone(),
        queue,
    ));
    let mut session = Session {
        ga: false,
        model: query.model,
        language: None,
        prompt: None,
        vad: Some(DEFAULT_VAD),
    };
    let mut buffer = Buffer::new(session.vad, 0);
    connection
        .send(
            "transcription_session.created",
            json!({"session": session.view(&id)}),
        )
        .await;
    while let Some(Ok(message)) = stream.next().await {
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            Message::Binary(_) => {
                connection
                    .error("unsupported_message", None, "Send JSON text events.", None)
                    .await;
                continue;
            }
            _ => continue,
        };
        let Ok(event) = serde_json::from_str::<Value>(&text) else {
            connection
                .error("invalid_json", None, "Events must be JSON objects.", None)
                .await;
            continue;
        };
        let client_event = event
            .get("event_id")
            .and_then(Value::as_str)
            .filter(|id| valid_text(id, 128));
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "transcription_session.update" | "session.update" => {
                let previous_vad = session.vad;
                match session.update(&event["session"]) {
                    Ok(()) => {
                        session.ga = kind == "session.update"
                            && (event["session"].get("audio").is_some()
                                || event["session"].get("type").is_some());
                        if session.vad != previous_vad {
                            let mut next = Buffer::new(session.vad, buffer.end_ms());
                            next.samples = std::mem::take(&mut buffer.samples);
                            next.origin_ms = buffer.origin_ms;
                            next.previous = buffer.previous.take();
                            buffer = next;
                        }
                        let reply = if kind == "session.update" {
                            "session.updated"
                        } else {
                            "transcription_session.updated"
                        };
                        connection
                            .send(reply, json!({"session": session.view(&id)}))
                            .await;
                    }
                    Err(error) => {
                        connection
                            .error(error.code, Some(error.param), error.message, client_event)
                            .await
                    }
                }
            }
            "input_audio_buffer.append" => {
                let decoded = event
                    .get("audio")
                    .and_then(Value::as_str)
                    .and_then(|audio| base64::engine::general_purpose::STANDARD.decode(audio).ok());
                let Some(bytes) = decoded else {
                    connection
                        .error(
                            "invalid_audio",
                            Some("audio"),
                            "audio must be base64 pcm16 data.",
                            client_event,
                        )
                        .await;
                    continue;
                };
                append(
                    &connection,
                    &jobs,
                    &session,
                    &mut buffer,
                    audio::from_le_bytes(&bytes),
                    client_event,
                )
                .await;
            }
            "input_audio_buffer.commit" => {
                if buffer.samples.len() < MIN_COMMIT_SAMPLES {
                    connection
                        .error(
                            "input_audio_buffer_commit_empty",
                            None,
                            "Commit at least 100 ms of buffered audio.",
                            client_event,
                        )
                        .await;
                    continue;
                }
                let from = buffer.speech_start_ms.unwrap_or(0);
                commit(&connection, &jobs, &session, &mut buffer, from).await;
            }
            "input_audio_buffer.clear" => {
                buffer.reset(session.vad);
                connection
                    .send("input_audio_buffer.cleared", json!({}))
                    .await;
            }
            _ => {
                connection
                    .error(
                        "unsupported_event",
                        Some("type"),
                        "This event type is not supported by transcription sessions.",
                        client_event,
                    )
                    .await;
            }
        }
    }
    closed.store(true, Ordering::Release);
    drop(jobs);
    drop(connection);
    let _ = worker.await;
    let _ = writer.await;
    drop(slot);
}

fn random_tag() -> u64 {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    u64::from_le_bytes(bytes) & 0xffff_ffff_0000
}

async fn append(
    connection: &Connection,
    jobs: &mpsc::Sender<Job>,
    session: &Session,
    buffer: &mut Buffer,
    samples: Vec<i16>,
    client_event: Option<&str>,
) {
    if buffer.samples.len() + samples.len() > MAX_BUFFER_SAMPLES {
        if buffer.speech_start_ms.is_some() {
            let from = buffer.speech_start_ms.unwrap_or(0);
            commit(connection, jobs, session, buffer, from).await;
        } else if session.vad.is_none() {
            connection
                .error(
                    "input_audio_buffer_full",
                    Some("audio"),
                    "The input audio buffer is full. Commit or clear it.",
                    client_event,
                )
                .await;
            return;
        }
    }
    let events = match buffer.vad.as_mut() {
        Some(vad) => vad.push(&samples),
        None => Vec::new(),
    };
    buffer.samples.extend_from_slice(&samples);
    for event in events {
        match event {
            audio::VadEvent::Started { at_ms } => {
                let at = buffer.vad_origin_ms + at_ms;
                let item = connection.item();
                buffer.speech_start_ms = Some(at.max(buffer.origin_ms));
                buffer.item = Some(item.clone());
                connection
                    .send(
                        "input_audio_buffer.speech_started",
                        json!({"audio_start_ms": at, "item_id": item}),
                    )
                    .await;
            }
            audio::VadEvent::Stopped { at_ms } => {
                let at = buffer.vad_origin_ms + at_ms;
                let item = buffer.item.clone().unwrap_or_else(|| connection.item());
                buffer.item = Some(item.clone());
                connection
                    .send(
                        "input_audio_buffer.speech_stopped",
                        json!({"audio_end_ms": at, "item_id": item}),
                    )
                    .await;
                let from = buffer.speech_start_ms.unwrap_or(buffer.origin_ms);
                commit(connection, jobs, session, buffer, from).await;
            }
        }
    }
    if let Some(vad) = session.vad {
        buffer.trim_idle(vad.prefix_padding_ms);
    }
}

async fn commit(
    connection: &Connection,
    jobs: &mpsc::Sender<Job>,
    session: &Session,
    buffer: &mut Buffer,
    from_ms: u64,
) {
    let item = buffer.item.take().unwrap_or_else(|| connection.item());
    let samples = buffer.take(from_ms);
    if let Some(vad) = buffer.vad.as_mut() {
        vad.reset();
    }
    connection
        .send(
            "input_audio_buffer.committed",
            json!({"previous_item_id": buffer.previous, "item_id": item}),
        )
        .await;
    buffer.previous = Some(item.clone());
    let job = Job {
        item: item.clone(),
        samples,
        model: session.model.clone(),
        language: session.language.clone(),
    };
    if jobs.try_send(job).is_err() {
        connection
            .send(
                "conversation.item.input_audio_transcription.failed",
                json!({"item_id": item, "content_index": 0, "error": {"type": "transcription_error", "code": "transcription_backlog", "message": "Too many transcriptions are pending for this session.", "param": null}}),
            )
            .await;
    }
}

async fn transcriber(
    manager: Arc<Manager>,
    connection: Arc<Connection>,
    closed: Arc<AtomicBool>,
    mut queue: mpsc::Receiver<Job>,
) {
    while let Some(job) = queue.recv().await {
        if closed.load(Ordering::Acquire) {
            continue;
        }
        let model = match job.model {
            Some(model) => Some(model),
            None => crate::voice::models(&manager, Operation::Transcription)
                .await
                .into_iter()
                .next(),
        };
        let samples = audio::resample(&job.samples, INPUT_RATE, audio::WHISPER_RATE);
        let result = match &model {
            Some(model) => {
                crate::voice::transcribe(
                    &manager,
                    model,
                    &samples,
                    crate::voice::language(job.language.as_deref()),
                )
                .await
            }
            None => Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_required",
                "Set input_audio_transcription.model or enable a transcription model.",
            )),
        };
        match result {
            Ok(transcript) => {
                connection
                    .send(
                        "conversation.item.input_audio_transcription.completed",
                        json!({"item_id": job.item, "content_index": 0, "transcript": transcript}),
                    )
                    .await
            }
            Err(error) => {
                connection
                    .send(
                        "conversation.item.input_audio_transcription.failed",
                        json!({"item_id": job.item, "content_index": 0, "error": {"type": "transcription_error", "code": error.code, "message": error.message, "param": null}}),
                    )
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            ga: false,
            model: None,
            language: None,
            prompt: None,
            vad: Some(DEFAULT_VAD),
        }
    }

    #[test]
    fn beta_and_ga_updates_parse_the_same_fields() {
        let mut beta = session();
        beta.update(&json!({"input_audio_format": "pcm16", "input_audio_transcription": {"model": "whisper", "language": "en"}, "turn_detection": {"type": "server_vad", "threshold": 0.7, "silence_duration_ms": 300}, "include": ["item.input_audio_transcription.logprobs"]})).ok().unwrap();
        assert_eq!(beta.model.as_deref(), Some("whisper"));
        assert_eq!(beta.vad.unwrap().threshold, 0.7);
        assert_eq!(beta.vad.unwrap().silence_ms, 300);
        assert_eq!(beta.vad.unwrap().prefix_padding_ms, 300);
        let mut ga = session();
        ga.ga = true;
        ga.update(&json!({"type": "transcription", "audio": {"input": {"format": {"type": "audio/pcm", "rate": 24000}, "transcription": {"model": "whisper"}, "turn_detection": null}}})).ok().unwrap();
        assert_eq!(ga.model.as_deref(), Some("whisper"));
        assert!(ga.vad.is_none());
        assert_eq!(
            ga.view("s")["audio"]["input"]["turn_detection"],
            Value::Null
        );
        assert_eq!(beta.view("s")["input_audio_format"], "pcm16");
    }

    #[test]
    fn invalid_updates_leave_the_session_unchanged() {
        let original = session();
        for update in [
            json!({"input_audio_format": "g711_ulaw"}),
            json!({"audio": {"input": {"format": {"type": "audio/pcm", "rate": 16000}}}}),
            json!({"turn_detection": {"type": "semantic_vad"}}),
            json!({"turn_detection": {"threshold": 2}}),
            json!({"input_audio_transcription": {"model": 1}}),
            json!({"input_audio_transcription": {"model": "ok"}, "turn_detection": {"silence_duration_ms": 0}}),
            json!("text"),
        ] {
            let mut changed = original.clone();
            assert!(changed.update(&update).is_err(), "{update}");
            assert_eq!(changed, original);
        }
    }

    #[test]
    fn idle_trimming_keeps_prefix_padding_and_clock() {
        let mut buffer = Buffer::new(Some(DEFAULT_VAD), 0);
        buffer.samples = vec![0; INPUT_RATE as usize * 10];
        buffer.trim_idle(300);
        assert_eq!(buffer.samples.len(), INPUT_RATE as usize * 4 / 10);
        assert_eq!(buffer.origin_ms, 9600);
        assert_eq!(buffer.end_ms(), 10_000);
        buffer.speech_start_ms = Some(9_800);
        buffer.samples.extend(vec![1; INPUT_RATE as usize]);
        let taken = buffer.take(9_800);
        assert_eq!(taken.len(), INPUT_RATE as usize * 12 / 10);
        assert_eq!(buffer.origin_ms, 11_000);
    }
}
