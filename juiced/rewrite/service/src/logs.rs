use serde::Serialize;
use std::{
    collections::VecDeque,
    io::Read,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

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
        })
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
        inner.bytes += line_cost(&line);
        inner.lines.push_back(line);
        while inner.lines.len() > self.capacity_lines || inner.bytes > self.capacity_bytes {
            let Some(evicted) = inner.lines.pop_front() else {
                break;
            };
            inner.bytes -= line_cost(&evicted);
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
