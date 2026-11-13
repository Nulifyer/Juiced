use serde::Serialize;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

pub const CAPACITY_LINES: usize = 4096;
pub const CAPACITY_BYTES: usize = 8 * 1024 * 1024;
pub const LINE_BYTES: usize = 4096;
pub const DEFAULT_PAGE: usize = 200;
pub const MAXIMUM_PAGE: usize = 500;
const READ_CHUNK: usize = 8192;
const READER_STACK: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Info,
    Debug,
}

impl Level {
    fn rank(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warning => 2,
            Self::Info => 3,
            Self::Debug => 4,
        }
    }

    fn severity(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warning => "Warn",
            Self::Info => "Info",
            Self::Debug => "Debug",
        }
    }
}

/// The most verbose level mirrored to stderr; `None` mirrors nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Threshold(Option<Level>);

impl Threshold {
    pub const DEFAULT: Self = Self(Some(Level::Info));
    pub const OFF: Self = Self(None);

    /// Accepts the native `log_level` names.
    pub fn parse(text: &str) -> Option<Self> {
        Some(Self(match text.trim().to_ascii_lowercase().as_str() {
            "trace" | "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warning" | "warn" => Some(Level::Warning),
            "error" | "fatal" => Some(Level::Error),
            "none" => None,
            _ => return None,
        }))
    }

    /// `LEMONADE_LOG_LEVEL` overrides the configured `log_level`. An unusable environment
    /// value is reported and ignored rather than silencing the service.
    pub fn resolve(configured: Option<&str>) -> (Self, Option<String>) {
        let configured = configured.and_then(Self::parse).unwrap_or(Self::DEFAULT);
        match std::env::var("LEMONADE_LOG_LEVEL") {
            Ok(value) if !value.trim().is_empty() => match Self::parse(&value) {
                Some(level) => (level, None),
                None => (
                    configured,
                    Some(format!(
                        "Ignoring LEMONADE_LOG_LEVEL={value:?}; use trace, debug, info, warning, error, fatal or none."
                    )),
                ),
            },
            _ => (configured, None),
        }
    }

    fn encode(self) -> u8 {
        self.0.map_or(0, Level::rank)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Service,
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Service,
    Worker { model: Arc<str>, generation: u64 },
}

#[derive(Clone, Debug, Serialize)]
pub struct Line {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub source: Arc<str>,
    pub model: Option<Arc<str>>,
    pub generation: Option<u64>,
    pub stream: Stream,
    pub level: Option<Level>,
    pub text: String,
    pub truncated: bool,
}

impl Line {
    fn severity(&self) -> &'static str {
        self.level.unwrap_or(Level::Info).severity()
    }

    pub fn timestamp(&self) -> String {
        format_timestamp(self.timestamp_ms)
    }

    /// The native single-line rendering, as written to the container log.
    pub fn render(&self) -> String {
        format!(
            "{} [{}] ({}) {}",
            self.timestamp(),
            self.severity(),
            self.source,
            self.text
        )
    }

    /// The native `/logs/stream` entry shape.
    pub fn entry(&self) -> serde_json::Value {
        serde_json::json!({
            "seq": self.seq,
            "timestamp": self.timestamp(),
            "severity": self.severity(),
            "tag": self.source,
            "line": self.render(),
        })
    }
}

fn format_timestamp(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let days = (seconds / 86_400) as i64;
    let of_day = seconds % 86_400;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60,
        timestamp_ms % 1000
    )
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub data: Vec<Line>,
    pub next: u64,
    pub dropped: u64,
    pub oldest: Option<u64>,
    pub latest: u64,
    pub cursor_reset: bool,
}

struct Inner {
    lines: VecDeque<Line>,
    bytes: usize,
    next_seq: u64,
}

pub struct LogBuffer {
    capacity_lines: usize,
    capacity_bytes: usize,
    service: Arc<str>,
    inner: Mutex<Inner>,
    mirror: AtomicU8,
    published: watch::Sender<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(u64::MAX as u128) as u64
        })
}

fn line_cost(line: &Line) -> usize {
    line.text.len() + std::mem::size_of::<Line>()
}

impl LogBuffer {
    pub fn new() -> Arc<Self> {
        Self::with_capacity(CAPACITY_LINES, CAPACITY_BYTES)
    }

    pub fn with_capacity(lines: usize, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity_lines: lines.max(1),
            capacity_bytes: bytes.max(LINE_BYTES + std::mem::size_of::<Line>()),
            service: Arc::from("service"),
            inner: Mutex::new(Inner {
                lines: VecDeque::new(),
                bytes: 0,
                next_seq: 1,
            }),
            mirror: AtomicU8::new(Threshold::OFF.encode()),
            published: watch::channel(0).0,
        })
    }

    pub fn mirror_to_stderr(&self, threshold: Threshold) {
        self.mirror.store(threshold.encode(), Ordering::Relaxed);
    }

    /// Changes whenever a line is retained; the value is the latest sequence number.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.published.subscribe()
    }

    pub fn service(&self, level: Level, message: &str) {
        self.push(
            &Origin::Service,
            Stream::Service,
            Some(level),
            message.as_bytes(),
            false,
        );
    }

    pub fn push(
        &self,
        origin: &Origin,
        stream: Stream,
        level: Option<Level>,
        raw: &[u8],
        truncated: bool,
    ) -> Option<u64> {
        let (text, clipped) = sanitize(raw);
        if text.is_empty() {
            return None;
        }
        let level = level.or_else(|| parse_level(&text));
        let (source, model, generation) = match origin {
            Origin::Service => (self.service.clone(), None, None),
            Origin::Worker { model, generation } => {
                (model.clone(), Some(model.clone()), Some(*generation))
            }
        };
        let timestamp_ms = now_ms();
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let seq = inner.next_seq;
        inner.next_seq = seq.saturating_add(1);
        let line = Line {
            seq,
            timestamp_ms,
            source,
            model,
            generation,
            stream,
            level,
            text,
            truncated: truncated || clipped,
        };
        let mirrored = (line.level.unwrap_or(Level::Info).rank()
            <= self.mirror.load(Ordering::Relaxed))
        .then(|| line.render());
        inner.bytes += line_cost(&line);
        inner.lines.push_back(line);
        while inner.lines.len() > self.capacity_lines || inner.bytes > self.capacity_bytes {
            let Some(evicted) = inner.lines.pop_front() else {
                break;
            };
            inner.bytes -= line_cost(&evicted);
        }
        drop(inner);
        self.published.send_replace(seq);
        if let Some(mut rendered) = mirrored {
            rendered.push('\n');
            let _ = std::io::stderr().lock().write_all(rendered.as_bytes());
        }
        Some(seq)
    }

    pub fn read(&self, after: u64, limit: usize, source: Option<&str>) -> Page {
        let limit = limit.clamp(1, MAXIMUM_PAGE);
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let latest = inner.next_seq - 1;
        let oldest = inner.lines.front().map(|line| line.seq);
        let cursor_reset = after > latest;
        let after = if cursor_reset { 0 } else { after };
        let first_retained = oldest.unwrap_or(inner.next_seq);
        let dropped = first_retained.saturating_sub(after.saturating_add(1));
        let start = inner.lines.partition_point(|line| line.seq <= after);
        let mut data = Vec::new();
        let mut next = after.max(first_retained.saturating_sub(1));
        for line in inner.lines.range(start..) {
            if data.len() == limit {
                break;
            }
            next = line.seq;
            if source.is_none_or(|source| *line.source == *source) {
                data.push(line.clone());
            }
        }
        Page {
            data,
            next,
            dropped,
            oldest,
            latest,
            cursor_reset,
        }
    }

    pub fn capture<R: Read + Send + 'static>(
        self: &Arc<Self>,
        origin: Origin,
        stream: Stream,
        reader: R,
    ) -> std::io::Result<()> {
        let buffer = self.clone();
        let name = match stream {
            Stream::Stderr => "juiced-log-stderr",
            _ => "juiced-log-stdout",
        };
        std::thread::Builder::new()
            .name(name.into())
            .stack_size(READER_STACK)
            .spawn(move || drain(&buffer, &origin, stream, reader))
            .map(drop)
    }
}

/// Serves the native `/logs/stream` protocol: nothing is sent until `logs.subscribe`, which
/// answers with one `logs.snapshot` of the retained lines and then a `logs.entry` per new line.
/// `accepts` validates the token of an in-band `auth` message.
pub async fn stream(
    buffer: Arc<LogBuffer>,
    mut socket: axum::extract::ws::WebSocket,
    accepts: impl Fn(&str) -> bool + Send,
) {
    use axum::extract::ws::Message;
    let mut updates = buffer.subscribe();
    let mut cursor: Option<u64> = None;
    loop {
        let outgoing = tokio::select! {
            message = socket.recv() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => {
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                    _ => break,
                };
                match client_message(&text, &accepts) {
                    Ok(Some(after)) => {
                        let (entries, next) = collect(&buffer, after.unwrap_or(0), usize::MAX);
                        cursor = Some(next);
                        vec![serde_json::json!({"type": "logs.snapshot", "entries": entries})]
                    }
                    Ok(None) => vec![serde_json::json!({"type": "auth.ok"})],
                    Err(message) => vec![serde_json::json!({"type": "error", "error": {"message": message, "type": "invalid_request_error"}})],
                }
            }
            changed = updates.changed(), if cursor.is_some() => {
                if changed.is_err() {
                    break;
                }
                let (entries, next) = collect(&buffer, cursor.unwrap_or(0), MAXIMUM_PAGE);
                cursor = Some(next);
                if next < *updates.borrow() {
                    updates.mark_changed();
                }
                entries
                    .into_iter()
                    .map(|entry| serde_json::json!({"type": "logs.entry", "entry": entry}))
                    .collect()
            }
        };
        for message in outgoing {
            if socket
                .send(Message::Text(message.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// `Ok(Some(after_seq))` subscribes, `Ok(None)` acknowledges a valid `auth` message.
fn client_message(
    text: &str,
    accepts: &impl Fn(&str) -> bool,
) -> Result<Option<Option<u64>>, String> {
    let request: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("Invalid JSON: {error}"))?;
    match request["type"].as_str().unwrap_or_default() {
        "logs.subscribe" => match &request["after_seq"] {
            serde_json::Value::Null => Ok(Some(None)),
            value => value
                .as_u64()
                .map(|after| Some(Some(after)))
                .ok_or_else(|| "after_seq must be null or a non-negative integer".to_owned()),
        },
        "auth" => {
            let token = request["token"]
                .as_str()
                .filter(|token| !token.is_empty())
                .or_else(|| request["api_key"].as_str())
                .unwrap_or_default();
            let token = token.strip_prefix("Bearer ").unwrap_or(token);
            if accepts(token) {
                Ok(None)
            } else {
                Err("Invalid API key".to_owned())
            }
        }
        _ => Err("Expected logs.subscribe message".to_owned()),
    }
}

fn collect(buffer: &LogBuffer, mut after: u64, limit: usize) -> (Vec<serde_json::Value>, u64) {
    let mut entries = Vec::new();
    loop {
        let page = buffer.read(after, MAXIMUM_PAGE, None);
        let full = page.data.len() == MAXIMUM_PAGE;
        entries.extend(page.data.iter().map(Line::entry));
        let advanced = page.next != after || page.cursor_reset;
        after = page.next;
        if !full || !advanced || entries.len() >= limit {
            return (entries, after);
        }
    }
}

pub(crate) fn drain(buffer: &LogBuffer, origin: &Origin, stream: Stream, mut reader: impl Read) {
    let mut chunk = vec![0u8; READ_CHUNK];
    let mut line = Vec::with_capacity(256);
    let mut overflowed = false;
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &byte in &chunk[..count] {
            if byte == b'\n' || byte == b'\r' {
                if !line.is_empty() || overflowed {
                    buffer.push(origin, stream, None, &line, overflowed);
                }
                line.clear();
                overflowed = false;
            } else if line.len() < LINE_BYTES {
                line.push(byte);
            } else {
                overflowed = true;
            }
        }
    }
    if !line.is_empty() || overflowed {
        buffer.push(origin, stream, None, &line, overflowed);
    }
}

fn sanitize(raw: &[u8]) -> (String, bool) {
    let clipped = raw.len() > LINE_BYTES;
    let raw = &raw[..raw.len().min(LINE_BYTES)];
    let mut cleaned = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        let byte = raw[index];
        if byte == 0x1b {
            index += 1;
            if raw.get(index) == Some(&b'[') {
                index += 1;
                while index < raw.len() && !(0x40..=0x7e).contains(&raw[index]) {
                    index += 1;
                }
            }
            index += 1;
            continue;
        }
        if byte == b'\t' || (byte >= 0x20 && byte != 0x7f) {
            cleaned.push(byte);
        }
        index += 1;
    }
    let text = String::from_utf8_lossy(&cleaned);
    let mut text = text.trim_end().to_owned();
    while text.len() > LINE_BYTES {
        text.pop();
    }
    (text, clipped)
}

pub fn parse_level(text: &str) -> Option<Level> {
    for token in text.split_whitespace().take(3) {
        let token = token
            .trim_matches(|c: char| matches!(c, '[' | ']' | ':' | '(' | ')' | '<' | '>'))
            .to_ascii_lowercase();
        let token = token.strip_prefix("level=").unwrap_or(&token);
        let level = match token {
            "e" | "err" | "error" | "fatal" | "critical" | "crit" => Level::Error,
            "w" | "warn" | "warning" => Level::Warning,
            "i" | "info" => Level::Info,
            "d" | "debug" | "trace" => Level::Debug,
            _ => continue,
        };
        return Some(level);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> Origin {
        Origin::Worker {
            model: Arc::from("model.one"),
            generation: 7,
        }
    }

    #[test]
    fn cursor_pages_and_reports_lines_that_fell_out_of_the_ring() {
        let buffer = LogBuffer::with_capacity(4, CAPACITY_BYTES);
        for index in 1..=3 {
            buffer.push(
                &worker(),
                Stream::Stdout,
                None,
                format!("line {index}").as_bytes(),
                false,
            );
        }
        let first = buffer.read(0, 2, None);
        assert_eq!(
            first.data.iter().map(|line| line.seq).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!((first.next, first.dropped, first.latest), (2, 0, 3));
        let second = buffer.read(first.next, 10, None);
        assert_eq!(second.data.len(), 1);
        assert_eq!(second.next, 3);
        let empty = buffer.read(second.next, 10, None);
        assert!(empty.data.is_empty());
        assert_eq!(empty.next, 3);
        for index in 4..=10 {
            buffer.push(
                &worker(),
                Stream::Stderr,
                None,
                format!("line {index}").as_bytes(),
                false,
            );
        }
        let behind = buffer.read(3, 10, None);
        assert_eq!(behind.oldest, Some(7));
        assert_eq!(behind.dropped, 3);
        assert_eq!(
            behind.data.iter().map(|line| line.seq).collect::<Vec<_>>(),
            [7, 8, 9, 10]
        );
        assert_eq!(behind.next, 10);
        assert_eq!(buffer.read(0, 10, None).dropped, 6);
        let ahead = buffer.read(99, 10, None);
        assert!(ahead.cursor_reset);
        assert_eq!(ahead.data.len(), 4);
    }

    #[test]
    fn source_filter_advances_the_cursor_past_other_sources() {
        let buffer = LogBuffer::new();
        buffer.service(Level::Info, "service started");
        buffer.push(&worker(), Stream::Stdout, None, b"worker line", false);
        buffer.service(Level::Warning, "service warning");
        let page = buffer.read(0, 10, Some("service"));
        assert_eq!(
            page.data.iter().map(|line| line.seq).collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(page.next, 3);
        let worker_page = buffer.read(0, 10, Some("model.one"));
        assert_eq!(worker_page.data.len(), 1);
        assert_eq!(worker_page.data[0].generation, Some(7));
        assert_eq!(worker_page.data[0].stream, Stream::Stdout);
        assert_eq!(buffer.read(0, 10, Some("absent")).next, 3);
    }

    #[test]
    fn byte_budget_bounds_retained_text_independently_of_line_count() {
        let per_line = LINE_BYTES + std::mem::size_of::<Line>();
        let buffer = LogBuffer::with_capacity(1000, per_line * 3);
        for _ in 0..10 {
            buffer.push(&worker(), Stream::Stdout, None, &[b'x'; LINE_BYTES], false);
        }
        let page = buffer.read(0, MAXIMUM_PAGE, None);
        assert_eq!(page.data.len(), 3);
        assert_eq!(page.dropped, 7);
    }

    #[test]
    fn drained_lines_are_split_truncated_and_sanitized() {
        let buffer = LogBuffer::new();
        let mut input = b"first\r\nsecond\rthird\n\n\x1b[31mE colored\x1b[0m\n".to_vec();
        input.extend(std::iter::repeat_n(b'y', LINE_BYTES * 3));
        input.extend(b"\nafter long\nunterminated");
        drain(&buffer, &worker(), Stream::Stderr, &input[..]);
        let page = buffer.read(0, MAXIMUM_PAGE, None);
        let texts: Vec<_> = page.data.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts[..4], ["first", "second", "third", "E colored"]);
        assert_eq!(page.data[3].level, Some(Level::Error));
        assert_eq!(texts[4].len(), LINE_BYTES);
        assert!(page.data[4].truncated);
        assert!(!page.data[0].truncated);
        assert_eq!(texts[5..], ["after long", "unterminated"]);
        let mut invalid = vec![0xff, b'o', b'k', 0x00, 0x07];
        invalid.extend(std::iter::repeat_n(0xe2, 2));
        buffer.push(&worker(), Stream::Stdout, None, &invalid, false);
        let last = buffer.read(page.next, 1, None).data.remove(0);
        assert!(last.text.starts_with("\u{fffd}ok"));
        assert!(!last.text.contains('\u{0}'));
    }

    #[test]
    fn oversized_direct_pushes_are_clipped_to_the_line_limit() {
        let buffer = LogBuffer::new();
        let text = "é".repeat(LINE_BYTES);
        buffer.push(
            &Origin::Service,
            Stream::Service,
            Some(Level::Info),
            text.as_bytes(),
            false,
        );
        let line = buffer.read(0, 1, None).data.remove(0);
        assert!(line.truncated);
        assert!(line.text.len() <= LINE_BYTES);
        assert_eq!(line.source.as_ref(), "service");
        assert_eq!(line.level, Some(Level::Info));
    }

    #[test]
    fn native_rendering_uses_utc_timestamps_and_native_level_names() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00.000");
        assert_eq!(format_timestamp(951_782_400_123), "2000-02-29 00:00:00.123");
        assert_eq!(
            format_timestamp(1_794_540_104_515),
            "2026-11-13 03:21:44.515"
        );
        let buffer = LogBuffer::new();
        buffer.push(&worker(), Stream::Stderr, None, b"W slot busy", false);
        buffer.push(&worker(), Stream::Stdout, None, b"plain", false);
        let page = buffer.read(0, 10, None);
        let entry = page.data[0].entry();
        assert_eq!(entry["severity"], "Warn");
        assert_eq!(entry["tag"], "model.one");
        assert!(
            entry["line"]
                .as_str()
                .unwrap()
                .ends_with(" [Warn] (model.one) W slot busy")
        );
        assert_eq!(page.data[1].entry()["severity"], "Info");
        assert_eq!(
            Threshold::parse("Warning"),
            Some(Threshold(Some(Level::Warning)))
        );
        assert_eq!(
            Threshold::parse("trace"),
            Some(Threshold(Some(Level::Debug)))
        );
        assert_eq!(Threshold::parse("none"), Some(Threshold::OFF));
        assert_eq!(Threshold::parse("verbose"), None);
    }

    #[test]
    fn levels_are_parsed_only_from_leading_tokens() {
        assert_eq!(parse_level("W slot 0: low memory"), Some(Level::Warning));
        assert_eq!(parse_level("[ERROR] failed"), Some(Level::Error));
        assert_eq!(parse_level("0.01.234.567 I main: ready"), Some(Level::Info));
        assert_eq!(parse_level("level=debug msg=x"), Some(Level::Debug));
        assert_eq!(parse_level("srv load_model: loading"), None);
        assert_eq!(parse_level("the request had an error"), None);
    }

    #[test]
    fn a_full_pipe_is_drained_without_blocking_the_writer() {
        let buffer = LogBuffer::new();
        let (reader, mut writer) = std::io::pipe().unwrap();
        buffer
            .capture(worker(), Stream::Stdout, reader)
            .expect("start reader");
        let line = format!("{}\n", "z".repeat(1000));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for _ in 0..20_000 {
            std::io::Write::write_all(&mut writer, line.as_bytes()).unwrap();
            assert!(std::time::Instant::now() < deadline);
        }
        drop(writer);
        let wait = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while buffer.read(0, 1, None).latest < 20_000 {
            assert!(std::time::Instant::now() < wait, "reader did not drain");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let page = buffer.read(0, 1, None);
        assert_eq!(page.oldest, Some(20_000 - CAPACITY_LINES as u64 + 1));
    }
}
