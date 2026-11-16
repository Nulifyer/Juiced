//! Who sent each inference request, request metadata that survives a restart, and the
//! operator's opt-in body capture. History never holds prompt or reply text. Captures live in
//! memory only and are never written to disk, logs or metrics.

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, header},
    response::Response,
};
use futures_util::{Stream, StreamExt, stream};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const HISTORY_FILE: &str = "request-history.json";
pub const HISTORY_RECORDS: usize = 1000;
pub const HISTORY_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const LISTED_RECORDS: usize = 128;
pub const CAPTURES: usize = 20;
pub const CAPTURE_BYTES: usize = 64 * 1024;
const HISTORY_FILE_BYTES: u64 = 8 * 1024 * 1024;
const STREAM_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct Origin {
    pub protocol: &'static str,
    pub key: Option<Arc<str>>,
    pub capture: Option<Arc<Pending>>,
}

tokio::task_local! {
    static ORIGIN: Origin;
}

impl Origin {
    pub fn current() -> Option<Self> {
        ORIGIN.try_with(Clone::clone).ok()
    }

    /// A request the service routes to itself (MCP tools, Barista) keeps its caller's origin.
    pub fn present() -> bool {
        ORIGIN.try_with(|_| ()).is_ok()
    }

    pub async fn scope<F: Future>(self, future: F) -> F::Output {
        ORIGIN.scope(self, future).await
    }
}

/// A browser sends `Sec-Fetch-Site: same-origin` only for pages Juiced served itself.
pub fn from_console(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"same-origin"))
}

pub struct History {
    records: Mutex<VecDeque<Value>>,
    dirty: AtomicBool,
    root: Option<crate::storage::DirectoryLease>,
}

fn completed_at(record: &Value) -> u64 {
    record["completed_at_ms"].as_u64().unwrap_or(0)
}

impl History {
    pub fn load(root: Option<crate::storage::DirectoryLease>, now_ms: u64) -> Self {
        let mut records: VecDeque<Value> = root
            .as_ref()
            .and_then(|root| root.read_bytes(HISTORY_FILE, HISTORY_FILE_BYTES).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|file| file["schema_version"] == 1)
            .and_then(|mut file| match file["records"].take() {
                Value::Array(records) => Some(records),
                _ => None,
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|record| {
                record["id"].as_u64().is_some()
                    && record["model"].is_string()
                    && record.get("request_body").is_none()
            })
            .collect();
        records.make_contiguous().sort_by_key(completed_at);
        let history = Self {
            records: Mutex::new(records),
            dirty: AtomicBool::new(false),
            root,
        };
        history.prune(&mut history.lock(), now_ms);
        history
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Value>> {
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn prune(&self, records: &mut VecDeque<Value>, now_ms: u64) {
        let oldest = now_ms.saturating_sub(HISTORY_AGE_MS);
        while records.len() > HISTORY_RECORDS
            || records
                .front()
                .is_some_and(|record| completed_at(record) < oldest)
        {
            records.pop_front();
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Request IDs continue after the restored ones, so the console never sees two rows share one.
    pub fn next_id(&self) -> u64 {
        self.lock()
            .iter()
            .filter_map(|record| record["id"].as_u64())
            .max()
            .map_or(1, |id| id.saturating_add(1))
    }

    pub fn push(&self, record: Value, now_ms: u64) {
        let mut records = self.lock();
        records.push_back(record);
        self.dirty.store(true, Ordering::Release);
        self.prune(&mut records, now_ms);
    }

    /// Newest first.
    pub fn list(&self, limit: usize) -> Vec<Value> {
        self.lock().iter().rev().take(limit).cloned().collect()
    }

    pub fn flush(&self) {
        let Some(root) = &self.root else {
            return;
        };
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let records: Vec<Value> = self.lock().iter().cloned().collect();
        let file = serde_json::json!({"schema_version": 1, "records": records});
        if root
            .write_json(HISTORY_FILE, &file)
            .is_err_and(|error| !error.committed())
        {
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub fn spawn_flusher(self: &Arc<Self>) {
        if self.root.is_none() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let history = Arc::downgrade(self);
        runtime.spawn(async move {
            let mut ticks = tokio::time::interval(std::time::Duration::from_secs(10));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let Some(history) = history.upgrade() else {
                    break;
                };
                if history.dirty.load(Ordering::Acquire) {
                    let _ = tokio::task::spawn_blocking(move || history.flush()).await;
                }
            }
        });
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub text: Option<String>,
    pub bytes: u64,
    pub truncated: bool,
    /// Streamed replies keep the text they carried, not the event framing.
    pub assembled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Capture {
    pub request_id: u64,
    pub request: Part,
    pub response: Part,
}

#[derive(Default)]
struct Buffer {
    content_type: Option<String>,
    textual: bool,
    bytes: Vec<u8>,
    total: u64,
    truncated: bool,
}

impl Buffer {
    fn new(content_type: Option<&str>) -> Self {
        Self {
            content_type: content_type.map(str::to_owned),
            textual: content_type.is_none_or(textual),
            ..Self::default()
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        if !self.textual {
            return;
        }
        let room = CAPTURE_BYTES.saturating_sub(self.bytes.len());
        if chunk.len() > room {
            self.truncated = true;
        }
        self.bytes
            .extend_from_slice(&chunk[..chunk.len().min(room)]);
    }

    fn part(&self) -> Part {
        Part {
            content_type: self.content_type.clone(),
            text: self
                .textual
                .then(|| String::from_utf8_lossy(&self.bytes).into_owned()),
            bytes: self.total,
            truncated: self.truncated,
            assembled: false,
        }
    }
}

fn textual(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/") || essence.ends_with("json") || essence.ends_with("+json")
}

fn streamed(content_type: &str) -> bool {
    let essence = content_type.split(';').next().unwrap_or("").trim();
    essence.eq_ignore_ascii_case("text/event-stream")
        || essence.eq_ignore_ascii_case("application/x-ndjson")
}

/// Reassembles streamed text from OpenAI, Ollama and Anthropic events.
#[derive(Default)]
struct Assembler {
    line: Vec<u8>,
    skipping: bool,
    content: String,
    reasoning: String,
    tools: String,
    total: u64,
    truncated: bool,
}

impl Assembler {
    fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        for &byte in chunk {
            if byte == b'\n' {
                if !self.skipping {
                    let line = std::mem::take(&mut self.line);
                    self.event(&line);
                }
                self.line.clear();
                self.skipping = false;
            } else if !self.skipping {
                if self.line.len() >= STREAM_LINE_BYTES {
                    self.line.clear();
                    self.skipping = true;
                    self.truncated = true;
                } else {
                    self.line.push(byte);
                }
            }
        }
    }

    fn event(&mut self, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let payload = line.strip_prefix(b"data:").unwrap_or(line);
        let Ok(event) = serde_json::from_slice::<Value>(payload.trim_ascii()) else {
            return;
        };
        let mut pieces: Vec<(u8, String)> = Vec::new();
        let mut add = |kind: u8, value: &Value| {
            if let Some(text) = value.as_str().filter(|text| !text.is_empty()) {
                pieces.push((kind, text.to_owned()));
            }
        };
        for choice in event["choices"].as_array().into_iter().flatten() {
            add(0, &choice["delta"]["content"]);
            add(0, &choice["text"]);
            add(1, &choice["delta"]["reasoning_content"]);
            add(1, &choice["delta"]["reasoning"]);
            for call in choice["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
            {
                add(2, &call["function"]["name"]);
                add(2, &call["function"]["arguments"]);
            }
        }
        add(0, &event["message"]["content"]);
        add(1, &event["message"]["thinking"]);
        add(0, &event["response"]);
        add(1, &event["thinking"]);
        for call in event["message"]["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
        {
            add(2, &Value::String(call["function"].to_string()));
        }
        match event["type"].as_str() {
            Some("content_block_delta") => {
                add(0, &event["delta"]["text"]);
                add(1, &event["delta"]["thinking"]);
                add(2, &event["delta"]["partial_json"]);
            }
            Some("response.output_text.delta") => add(0, &event["delta"]),
            Some("response.reasoning_text.delta") => add(1, &event["delta"]),
            Some("response.function_call_arguments.delta") => add(2, &event["delta"]),
            _ => {}
        }
        for (kind, text) in pieces {
            let used = self.content.len() + self.reasoning.len() + self.tools.len();
            let room = CAPTURE_BYTES.saturating_sub(used);
            let mut end = text.len().min(room);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if end < text.len() {
                self.truncated = true;
            }
            let target = match kind {
                0 => &mut self.content,
                1 => &mut self.reasoning,
                _ => &mut self.tools,
            };
            target.push_str(&text[..end]);
        }
    }

    fn part(&self, content_type: Option<String>) -> Part {
        let mut assembled = serde_json::Map::new();
        for (key, value) in [
            ("content", &self.content),
            ("reasoning", &self.reasoning),
            ("tool_calls", &self.tools),
        ] {
            if !value.is_empty() {
                assembled.insert(key.into(), Value::String(value.clone()));
            }
        }
        Part {
            content_type,
            text: Some(serde_json::to_string_pretty(&Value::Object(assembled)).unwrap_or_default()),
            bytes: self.total,
            truncated: self.truncated,
            assembled: true,
        }
    }
}

/// One request's capture while it runs. It is kept only if the request produced a record.
pub struct Pending {
    record: Mutex<Option<u64>>,
    request: Mutex<Buffer>,
}

impl Pending {
    pub fn attach(&self, id: u64) {
        let mut record = self
            .record
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if record.is_none() {
            *record = Some(id);
        }
    }
}

#[derive(Default)]
pub struct Captures {
    enabled: AtomicBool,
    entries: Mutex<VecDeque<Capture>>,
}

impl Captures {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        if !enabled {
            self.entries
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }

    pub fn get(&self, id: u64) -> Option<Capture> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|capture| capture.request_id == id)
            .cloned()
    }

    pub fn ids(&self) -> Vec<u64> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|capture| capture.request_id)
            .collect()
    }

    fn store(&self, capture: Capture) {
        if !self.enabled() {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.len() >= CAPTURES {
            entries.pop_front();
        }
        entries.push_back(capture);
    }

    /// A pending capture whose request body is copied as the handler reads it.
    pub fn begin(&self, request: axum::extract::Request) -> (axum::extract::Request, Arc<Pending>) {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let pending = Arc::new(Pending {
            record: Mutex::new(None),
            request: Mutex::new(Buffer::new(content_type.as_deref())),
        });
        let (parts, body) = request.into_parts();
        let copy = pending.clone();
        let body = body.into_data_stream().map(move |chunk| {
            if let Ok(bytes) = &chunk {
                copy.request
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(bytes);
            }
            chunk
        });
        (
            axum::extract::Request::from_parts(parts, Body::from_stream(body)),
            pending,
        )
    }

    /// Copies the reply as it streams and keeps the capture when the body ends or is dropped.
    pub fn finish(self: &Arc<Self>, response: Response, pending: Arc<Pending>) -> Response {
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let tap = Tap {
            captures: self.clone(),
            pending,
            stream: content_type.as_deref().is_some_and(streamed),
            buffer: Buffer::new(content_type.as_deref()),
            assembler: Assembler::default(),
            content_type,
        };
        let (parts, body) = response.into_parts();
        let tapped = tap_stream(body.into_data_stream(), tap);
        Response::from_parts(parts, Body::from_stream(tapped))
    }
}

struct Tap {
    captures: Arc<Captures>,
    pending: Arc<Pending>,
    stream: bool,
    buffer: Buffer,
    assembler: Assembler,
    content_type: Option<String>,
}

impl Tap {
    fn push(&mut self, chunk: &Bytes) {
        if self.stream {
            self.assembler.push(chunk);
        } else {
            self.buffer.push(chunk);
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        let Some(id) = *self
            .pending
            .record
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        else {
            return;
        };
        if self.stream {
            self.assembler.push(b"\n");
        }
        let response = if self.stream {
            self.assembler.part(self.content_type.take())
        } else {
            self.buffer.part()
        };
        let request = self
            .pending
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .part();
        self.captures.store(Capture {
            request_id: id,
            request,
            response,
        });
    }
}

fn tap_stream<S>(body: S, tap: Tap) -> impl Stream<Item = Result<Bytes, axum::Error>> + Send
where
    S: Stream<Item = Result<Bytes, axum::Error>> + Send + Unpin + 'static,
{
    stream::unfold((body, tap), |(mut body, mut tap)| async move {
        let chunk = body.next().await?;
        if let Ok(bytes) = &chunk {
            tap.push(bytes);
        }
        Some((chunk, (body, tap)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_replies_keep_their_text_within_the_cap() {
        let mut assembler = Assembler::default();
        assembler.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\",\"reasoning_content\":\"think\"}}]}\n");
        assembler.push(b"{\"message\":{\"content\":\"!\"}}\n");
        assembler.push(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"partial_json\":\"{\\\"a\\\":1}\"}}\ndata: [DONE]\n");
        let part = assembler.part(None);
        let value: Value = serde_json::from_str(part.text.as_deref().unwrap()).unwrap();
        assert_eq!(value["content"], "Hello!");
        assert_eq!(value["reasoning"], "think");
        assert_eq!(value["tool_calls"], "{\"a\":1}");
        assert!(part.assembled && !part.truncated);
        let mut large = Assembler::default();
        let piece = "é".repeat(CAPTURE_BYTES);
        large.push(
            format!(
                "data: {}\n",
                serde_json::json!({"choices":[{"delta":{"content":piece}}]})
            )
            .as_bytes(),
        );
        assert!(large.truncated);
        assert!(large.content.len() <= CAPTURE_BYTES);
    }

    #[test]
    fn buffers_cap_text_and_skip_binary_bodies() {
        let mut text = Buffer::new(Some("application/json"));
        text.push(&vec![b'a'; CAPTURE_BYTES + 10]);
        let part = text.part();
        assert!(part.truncated);
        assert_eq!(part.text.unwrap().len(), CAPTURE_BYTES);
        assert_eq!(part.bytes, (CAPTURE_BYTES + 10) as u64);
        let mut audio = Buffer::new(Some("multipart/form-data; boundary=x"));
        audio.push(b"RIFF");
        assert!(audio.part().text.is_none());
        assert_eq!(audio.part().bytes, 4);
    }

    #[test]
    fn captures_are_bounded_and_cleared_when_turned_off() {
        let captures = Captures::default();
        let capture = |id| Capture {
            request_id: id,
            request: Part::default(),
            response: Part::default(),
        };
        captures.store(capture(1));
        assert!(captures.get(1).is_none());
        captures.set_enabled(true);
        for id in 0..(CAPTURES as u64 + 5) {
            captures.store(capture(id));
        }
        assert_eq!(captures.ids().len(), CAPTURES);
        assert!(captures.get(0).is_none());
        captures.set_enabled(false);
        assert!(captures.ids().is_empty());
    }

    #[test]
    fn history_is_bounded_by_count_and_age() {
        let now = 10 * HISTORY_AGE_MS;
        let history = History::load(None, now);
        history.push(
            serde_json::json!({"id": 1, "model": "m", "completed_at_ms": now - HISTORY_AGE_MS - 1}),
            now,
        );
        assert!(history.list(10).is_empty());
        for id in 0..(HISTORY_RECORDS as u64 + 3) {
            history.push(
                serde_json::json!({"id": id + 2, "model": "m", "completed_at_ms": now}),
                now,
            );
        }
        let listed = history.list(usize::MAX);
        assert_eq!(listed.len(), HISTORY_RECORDS);
        assert_eq!(listed[0]["id"], HISTORY_RECORDS as u64 + 4);
        assert_eq!(history.next_id(), HISTORY_RECORDS as u64 + 5);
    }
}
