use crate::config::Operation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAXIMUM_UNITS: usize = 4096;
pub const PROMPT_BLOCK_BYTES: usize = 512;
pub const SESSION_HEADERS: [&str; 3] = [
    "x-juiced-session",
    "x-juiced-session-id",
    "x-juiced-thread-id",
];
pub const CACHE_HEADER: &str = "x-juiced-cache";
pub const NO_STORE: &str = "no-store";
pub const PROTOCOL_PREFIX: &str = "auto.";
pub const LINEAGE_UNITS: usize = 64;
const FINE_BLOCK_BYTES: usize = 256;
const FINE_BLOCKS: usize = 256;
const COARSE_BLOCK_BYTES: usize = 1024;
const COARSE_BLOCKS: usize = 960;
/// The shortest shared opening worth restoring a saved state for, about 250 tokens. A
/// restore measured about 50 ms plus 0.3 ms per MiB of state; with the margin below, an
/// engine reading 1000 tokens a second repays the smallest restore after 100 tokens.
pub const MINIMUM_SHARED_BYTES: u64 = 1024;
/// Fewer reused tokens than this are the opening every prompt of a model shares, such as its
/// template header, not a prompt the engine still held.
pub const REUSED_TOKENS_FLOOR: u64 = 64;
const TEXT_BYTES_PER_TOKEN: f64 = 4.0;
const RESTORE_FIXED_MS: f64 = 50.0;
const RESTORE_MARGIN: f64 = 2.0;
const ASSUMED_PREFILL_TOKENS_PER_SECOND: f64 = 1000.0;
const ASSUMED_RESTORE_BYTES_PER_SECOND: f64 = 512.0 * 1024.0 * 1024.0;
/// The share of two requests' bytes that their common leading messages must cover before the
/// newer saved state replaces the older one. At nine tenths, a client that continues the
/// replaced request re-reads at most a ninth of what the kept state already holds.
pub const SIBLING_SHARE: (u64, u64) = (9, 10);
const RESERVATION_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
const MINIMUM_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_BUDGET_BYTES: u64 = 128 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Off,
    Memory,
    #[default]
    Persistent,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub budget_bytes: u64,
    pub model_budget_bytes: u64,
    pub max_sessions: u32,
    pub save_delay_ms: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, Mode>,
    /// Saved sessions not used for this many days are deleted; unset keeps them until the
    /// budget needs their space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    /// Session digests that are never evicted or expired.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub retained: BTreeSet<String>,
}

pub const MAXIMUM_RETAINED: usize = 1024;

pub fn valid_session_digest(session: &str) -> bool {
    session.len() == 64 && session.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Persistent,
            budget_bytes: 8 * 1024 * 1024 * 1024,
            model_budget_bytes: 4 * 1024 * 1024 * 1024,
            max_sessions: 1024,
            save_delay_ms: 1000,
            models: BTreeMap::new(),
            retention_days: None,
            retained: BTreeSet::new(),
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !(MINIMUM_BUDGET_BYTES..=MAXIMUM_BUDGET_BYTES).contains(&self.budget_bytes)
            || !(MINIMUM_BUDGET_BYTES..=self.budget_bytes).contains(&self.model_budget_bytes)
        {
            return Err(
                "Session budgets need 64 MiB to 128 GiB, with the model budget no larger than the total.",
            );
        }
        if !(1..=8192).contains(&self.max_sessions) || self.save_delay_ms > 60_000 {
            return Err(
                "Session limits need 1 to 8192 sessions and a save delay up to 60 seconds.",
            );
        }
        if self.models.len() > 4096
            || self
                .models
                .keys()
                .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
        {
            return Err("Per-model session overrides need at most 4096 bounded model IDs.");
        }
        if self
            .retention_days
            .is_some_and(|days| !(1..=365).contains(&days))
        {
            return Err("Session retention needs 1 to 365 days.");
        }
        if self.retained.len() > MAXIMUM_RETAINED
            || !self
                .retained
                .iter()
                .all(|session| valid_session_digest(session))
        {
            return Err("Retain at most 1024 listed sessions.");
        }
        Ok(())
    }

    pub fn mode_for(&self, model: &str) -> Mode {
        self.models.get(model).copied().unwrap_or(self.mode)
    }
}

pub type UnitDigest = [u8; 32];
pub type BlockDigest = [u8; 8];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyKind {
    Header,
    Prefix,
}

impl KeyKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Header => "h",
            Self::Prefix => "p",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Key {
    pub kind: KeyKind,
    chain: Vec<UnitDigest>,
    bytes: Vec<u64>,
    /// Chained digests of the request's text in order, so two requests can be compared for
    /// a shared opening that ends inside a message.
    text: Vec<BlockDigest>,
    reply: bool,
}

impl Key {
    /// A session the client names. Its one unit counts as the whole request, so the named
    /// session's own state always outranks states that only share text with the request.
    pub fn header(session: &str, operation: Operation, body: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"juiced-session-header-v1\0");
        hash.update(session.as_bytes());
        Self {
            kind: KeyKind::Header,
            chain: vec![hash.finalize().into()],
            bytes: vec![u64::MAX],
            text: request(operation, body)
                .map_or_else(Vec::new, |request| text_chain(&request.text)),
            reply: false,
        }
    }

    pub fn prefix(operation: Operation, body: &[u8]) -> Option<Self> {
        let Request { units, text, reply } = request(operation, body)?;
        if units.is_empty() {
            return None;
        }
        let mut previous: UnitDigest = Sha256::new()
            .chain_update(b"juiced-conversation-v1\0")
            .chain_update(operation.path().as_bytes())
            .finalize()
            .into();
        let mut total = 0u64;
        let (chain, bytes) = units
            .into_iter()
            .take(MAXIMUM_UNITS)
            .map(|(unit, size)| {
                previous = Sha256::new()
                    .chain_update(previous)
                    .chain_update(unit)
                    .finalize()
                    .into();
                total = total.saturating_add(size);
                (previous, total)
            })
            .unzip();
        Some(Self {
            kind: KeyKind::Prefix,
            chain,
            bytes,
            text: text_chain(&text),
            reply,
        })
    }

    /// Whether the request already holds a reply or tool result, so it continues an earlier
    /// exchange whatever the cache still knows of it.
    pub fn carries_reply(&self) -> bool {
        self.reply
    }

    pub fn units(&self) -> usize {
        self.chain.len()
    }

    pub fn saved_digest(&self) -> String {
        hex(self.chain.last().expect("keys have at least one unit"))
    }

    pub fn candidates(&self) -> impl Iterator<Item = (usize, String)> + '_ {
        self.chain
            .iter()
            .enumerate()
            .map(|(index, digest)| (index + 1, hex(digest)))
    }

    pub fn ancestors(&self) -> impl Iterator<Item = String> + '_ {
        self.chain[..self.chain.len() - 1].iter().map(hex)
    }

    pub fn common_units(&self, other: &Self) -> usize {
        if self.kind != other.kind {
            return 0;
        }
        self.chain
            .iter()
            .zip(&other.chain)
            .take_while(|(left, right)| left == right)
            .count()
    }

    pub fn extends(&self, earlier: &Self) -> bool {
        self.common_units(earlier) == earlier.units()
    }

    /// Leading units and their request bytes that `self` shares with `other`.
    pub fn shared(&self, other: &Self) -> (usize, u64) {
        let units = self.common_units(other);
        (
            units,
            units.checked_sub(1).map_or(0, |last| self.bytes[last]),
        )
    }

    /// Leading text blocks shared with `other`. Unlike units, these also match when a message
    /// changed partway through, such as a system prompt that embeds live figures or one long
    /// document followed by a different question.
    pub fn text_blocks_shared(&self, other: &Self) -> usize {
        if self.kind != other.kind {
            return 0;
        }
        self.text
            .iter()
            .zip(&other.text)
            .take_while(|(left, right)| left == right)
            .count()
    }

    /// Request bytes that `self` and `other` open with in common: their whole leading
    /// messages, or their leading text when the first difference falls inside a message.
    pub fn shared_bytes(&self, other: &Self) -> u64 {
        self.shared(other)
            .1
            .max(text_bytes(self.text_blocks_shared(other)))
    }

    /// Request bytes up to and including the unit at `units`, counted from one.
    pub fn unit_bytes(&self, units: usize) -> u64 {
        units
            .checked_sub(1)
            .and_then(|last| self.bytes.get(last))
            .copied()
            .unwrap_or(0)
    }

    /// Whether a state saved for `self` can stand in for the state of `older`, a request that
    /// nothing continued: the two open alike, and what they share is at least `SIBLING_SHARE`
    /// of each request and long enough to be worth restoring.
    pub fn replaces_sibling(&self, older: &Self) -> bool {
        if self.kind != KeyKind::Prefix || older.kind != KeyKind::Prefix {
            return false;
        }
        let most_of_both = |shared: u64, mine: Option<u64>, theirs: Option<u64>| {
            let most = |total: u64| {
                shared.saturating_mul(SIBLING_SHARE.1) >= total.saturating_mul(SIBLING_SHARE.0)
            };
            shared >= MINIMUM_SHARED_BYTES
                && mine
                    .zip(theirs)
                    .is_some_and(|(mine, theirs)| most(mine) && most(theirs))
        };
        let (units, shared) = self.shared(older);
        // A lineage cut at its unit limit no longer knows the size of its whole request.
        let by_messages = units > 0
            && most_of_both(
                shared,
                self.bytes.last().copied(),
                older
                    .bytes
                    .last()
                    .copied()
                    .filter(|_| older.units() < LINEAGE_UNITS),
            );
        // Nor does text cut at its block limit. The text may run up to one block past its
        // last whole one.
        let whole =
            |key: &Self| (key.text.len() < TEXT_BLOCKS).then(|| text_bytes(key.text.len() + 1));
        by_messages
            || most_of_both(
                text_bytes(self.text_blocks_shared(older)),
                whole(self),
                whole(older),
            )
    }

    pub fn lineage(&self) -> Self {
        let units = self.chain.len().min(LINEAGE_UNITS);
        Self {
            kind: self.kind,
            chain: self.chain[..units].to_vec(),
            bytes: self.bytes[..units].to_vec(),
            text: self.text.clone(),
            reply: false,
        }
    }

    pub fn to_record(&self, single: bool) -> LineageRecord {
        use std::fmt::Write;
        LineageRecord {
            chain: self.chain.iter().map(hex).collect(),
            bytes: self.bytes.clone(),
            single,
            header: self.kind == KeyKind::Header,
            text: self.text.iter().flatten().fold(
                String::with_capacity(self.text.len() * 16),
                |mut text, byte| {
                    let _ = write!(text, "{byte:02x}");
                    text
                },
            ),
        }
    }

    pub fn from_record(record: &LineageRecord) -> Option<Self> {
        if record.chain.is_empty()
            || record.chain.len() > LINEAGE_UNITS
            || record.chain.len() != record.bytes.len()
            || record.text.len() > TEXT_BLOCKS * 16
            || !record.text.len().is_multiple_of(16)
        {
            return None;
        }
        let chain = record
            .chain
            .iter()
            .map(|digest| unhex(digest))
            .collect::<Option<Vec<_>>>()?;
        let text = record
            .text
            .as_bytes()
            .as_chunks::<16>()
            .0
            .iter()
            .map(|block| unhex(std::str::from_utf8(block).ok()?))
            .collect::<Option<Vec<BlockDigest>>>()?;
        Some(Self {
            kind: if record.header {
                KeyKind::Header
            } else {
                KeyKind::Prefix
            },
            chain,
            bytes: record.bytes.clone(),
            text,
            reply: false,
        })
    }
}

const TEXT_BLOCKS: usize = FINE_BLOCKS + COARSE_BLOCKS;

/// Request bytes that `blocks` leading text blocks cover.
fn text_bytes(blocks: usize) -> u64 {
    (blocks.min(FINE_BLOCKS) * FINE_BLOCK_BYTES
        + blocks.saturating_sub(FINE_BLOCKS) * COARSE_BLOCK_BYTES) as u64
}

/// What a worker has shown that re-reading text and restoring a state cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rates {
    pub prefill_tokens_per_second: Option<f64>,
    pub restore_bytes_per_second: Option<f64>,
}

/// Whether restoring a saved state of `state_bytes` pays for the `gained_bytes` of request
/// text it adds to what the slot already holds: re-reading that text must take at least
/// `RESTORE_MARGIN` times as long as the restore.
pub fn worth_restoring(gained_bytes: u64, state_bytes: u64, rates: Rates) -> bool {
    let positive =
        |rate: Option<f64>, assumed: f64| rate.filter(|rate| *rate > 0.0).unwrap_or(assumed);
    let reread_ms = gained_bytes as f64 / TEXT_BYTES_PER_TOKEN * 1000.0
        / positive(
            rates.prefill_tokens_per_second,
            ASSUMED_PREFILL_TOKENS_PER_SECOND,
        );
    let restore_ms = RESTORE_FIXED_MS
        + state_bytes as f64 * 1000.0
            / positive(
                rates.restore_bytes_per_second,
                ASSUMED_RESTORE_BYTES_PER_SECOND,
            );
    gained_bytes >= MINIMUM_SHARED_BYTES && reread_ms >= RESTORE_MARGIN * restore_ms
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LineageRecord {
    pub chain: Vec<String>,
    pub bytes: Vec<u64>,
    /// The state came from one request that continued no earlier one. Records written before
    /// this was tracked read as false, so their states are never replaced by a sibling.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub single: bool,
    /// The client named the session; `chain` then holds the name's digest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub header: bool,
    /// The request's text blocks, sixteen hex digits each. Records written before these were
    /// tracked have none and match by whole messages only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// What a request's headers ask of the conversation cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hint<'a> {
    /// The session the client names.
    pub session: Option<&'a str>,
    /// Nothing will continue this request: it reuses what the engine or the cache holds, but
    /// its own state is never saved.
    pub no_store: bool,
}

/// Marks a request the service sends itself that nothing will continue, such as a model check
/// or a Tune trial, so its state is never saved. No client can set it.
#[derive(Clone, Copy)]
pub(crate) struct OneShot;

fn unhex<const BYTES: usize>(text: &str) -> Option<[u8; BYTES]> {
    if text.len() != BYTES * 2 {
        return None;
    }
    let mut digest = [0u8; BYTES];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(digest)
}

pub fn hex(digest: &UnitDigest) -> String {
    use std::fmt::Write;
    digest
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

pub fn model_tag(model: &str) -> String {
    hex(&Sha256::digest(model.as_bytes()).into())[..16].to_owned()
}

fn operation_tag(operation: Operation) -> Option<&'static str> {
    match operation {
        Operation::Chat => Some("chat"),
        Operation::Completion => Some("completions"),
        Operation::Responses => Some("responses"),
        _ => None,
    }
}

pub fn protocol(operation: Operation, kind: KeyKind, model: &str) -> Option<String> {
    Some(format!(
        "{PROTOCOL_PREFIX}{}.{}.{}",
        operation_tag(operation)?,
        kind.tag(),
        model_tag(model)
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolTag<'a> {
    pub operation: &'a str,
    pub kind: KeyKind,
    pub model_tag: &'a str,
}

pub fn parse_protocol(protocol: &str) -> Option<ProtocolTag<'_>> {
    let mut parts = protocol.strip_prefix(PROTOCOL_PREFIX)?.split('.');
    let operation = parts
        .next()
        .filter(|operation| matches!(*operation, "chat" | "completions" | "responses"))?;
    let kind = match parts.next()? {
        "h" => KeyKind::Header,
        "p" => KeyKind::Prefix,
        _ => return None,
    };
    let model_tag = parts
        .next()
        .filter(|tag| tag.len() == 16 && tag.bytes().all(|byte| byte.is_ascii_hexdigit()))?;
    parts.next().is_none().then_some(ProtocolTag {
        operation,
        kind,
        model_tag,
    })
}

#[derive(Deserialize)]
struct PromptCache {
    cache_prompt: Option<bool>,
}

pub fn prompt_cache_disabled(body: &[u8]) -> bool {
    serde_json::from_slice::<PromptCache>(body)
        .ok()
        .and_then(|body| body.cache_prompt)
        == Some(false)
}

pub fn with_slot(body: &[u8], slot: u32) -> Option<Vec<u8>> {
    let mut object: serde_json::Map<String, Value> = serde_json::from_slice(body).ok()?;
    object.insert("id_slot".into(), Value::from(slot));
    serde_json::to_vec(&object).ok()
}

type Unit = (UnitDigest, u64);

struct Request {
    units: Vec<Unit>,
    text: Vec<u8>,
    reply: bool,
}

fn request(operation: Operation, body: &[u8]) -> Option<Request> {
    match operation {
        Operation::Chat => chat_units(body),
        Operation::Completion => completion_units(body),
        Operation::Responses => responses_units(body),
        _ => None,
    }
}

fn message_unit(message: &Value) -> Unit {
    (message_digest(message), canonical(message).len() as u64)
}

/// Fine blocks first, so a short shared opening is measured closely, then coarse ones, so a
/// megabyte of text still fits a bounded record.
fn text_chain(text: &[u8]) -> Vec<BlockDigest> {
    let (fine, rest) = text.split_at(text.len().min(FINE_BLOCKS * FINE_BLOCK_BYTES));
    let mut previous: UnitDigest = Sha256::digest(b"juiced-text-chain-v1\0").into();
    let fine = fine.as_chunks::<FINE_BLOCK_BYTES>().0.iter();
    let coarse = rest.as_chunks::<COARSE_BLOCK_BYTES>().0.iter();
    fine.map(<[u8; FINE_BLOCK_BYTES]>::as_slice)
        .chain(
            coarse
                .take(COARSE_BLOCKS)
                .map(<[u8; COARSE_BLOCK_BYTES]>::as_slice),
        )
        .map(|block| {
            previous = Sha256::new()
                .chain_update(previous)
                .chain_update(block)
                .finalize()
                .into();
            let mut digest = BlockDigest::default();
            digest.copy_from_slice(&previous[..8]);
            digest
        })
        .collect()
}

/// A message as the text the engine reads, in the order a template lays it out. Fields the
/// unit digest also normalizes are normalized alike, so the two agree on what is equal.
fn message_text(text: &mut Vec<u8>, message: &Value) {
    let Value::Object(fields) = message else {
        text.extend_from_slice(canonical(message).as_bytes());
        text.push(b'\n');
        return;
    };
    for name in ["role", "name", "content", "tool_calls", "tool_call_id"] {
        match fields.get(name) {
            None | Some(Value::Null) => continue,
            Some(Value::String(value)) => text.extend_from_slice(normalize(value).as_bytes()),
            Some(Value::Array(parts)) if name == "content" => match text_parts(parts) {
                Some(value) => text.extend_from_slice(normalize(&value).as_bytes()),
                None => text.extend_from_slice(canonical(&fields[name]).as_bytes()),
            },
            Some(value) => text.extend_from_slice(canonical(value).as_bytes()),
        }
        text.push(b'\n');
    }
}

fn chat_units(body: &[u8]) -> Option<Request> {
    #[derive(Deserialize)]
    struct Chat {
        messages: Vec<Value>,
    }
    let chat: Chat = serde_json::from_slice(body).ok()?;
    let mut text = Vec::new();
    for message in &chat.messages {
        message_text(&mut text, message);
    }
    Some(Request {
        units: chat.messages.iter().map(message_unit).collect(),
        text,
        reply: chat
            .messages
            .iter()
            .any(|message| matches!(message["role"].as_str(), Some("assistant" | "tool"))),
    })
}

fn responses_units(body: &[u8]) -> Option<Request> {
    #[derive(Deserialize)]
    struct Responses {
        input: Value,
        instructions: Option<String>,
    }
    let request: Responses = serde_json::from_slice(body).ok()?;
    let mut units = Vec::new();
    let mut text = Vec::new();
    if let Some(instructions) = &request.instructions {
        units.push((
            text_digest(b"instructions", instructions),
            instructions.len() as u64,
        ));
        text.extend_from_slice(normalize(instructions).as_bytes());
        text.push(b'\n');
    }
    let mut reply = false;
    match &request.input {
        Value::String(input) => {
            units.extend(text_blocks(input));
            text.extend_from_slice(input.as_bytes());
        }
        Value::Array(items) => {
            units.extend(items.iter().map(message_unit));
            for item in items {
                message_text(&mut text, item);
            }
            reply = items.iter().any(|item| {
                item["role"] == "assistant"
                    || item["type"].as_str().is_some_and(|kind| kind != "message")
            });
        }
        _ => return None,
    }
    Some(Request { units, text, reply })
}

fn completion_units(body: &[u8]) -> Option<Request> {
    #[derive(Deserialize)]
    struct Completion {
        prompt: Value,
    }
    let request: Completion = serde_json::from_slice(body).ok()?;
    match &request.prompt {
        Value::String(text) => Some(Request {
            units: text_blocks(text).collect(),
            text: text.as_bytes().to_vec(),
            reply: false,
        }),
        _ => None,
    }
}

fn text_blocks(text: &str) -> impl Iterator<Item = Unit> + '_ {
    text.as_bytes()
        .as_chunks::<PROMPT_BLOCK_BYTES>()
        .0
        .iter()
        .map(|block| {
            (
                Sha256::new()
                    .chain_update(b"juiced-prompt-block-v1\0")
                    .chain_update(block)
                    .finalize()
                    .into(),
                PROMPT_BLOCK_BYTES as u64,
            )
        })
}

fn text_digest(label: &[u8], text: &str) -> UnitDigest {
    let mut hash = Sha256::new();
    hash.update(b"juiced-text-v1\0");
    field(&mut hash, label);
    field(&mut hash, normalize(text).as_bytes());
    hash.finalize().into()
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_owned()
}

fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn message_digest(message: &Value) -> UnitDigest {
    let mut hash = Sha256::new();
    hash.update(b"juiced-message-v1\0");
    let Value::Object(fields) = message else {
        field(&mut hash, b"value");
        field(&mut hash, canonical(message).as_bytes());
        return hash.finalize().into();
    };
    for name in [
        "type",
        "role",
        "name",
        "content",
        "tool_calls",
        "tool_call_id",
    ] {
        field(&mut hash, name.as_bytes());
        match fields.get(name) {
            None | Some(Value::Null) => hash.update([0]),
            Some(Value::String(kind)) if name == "type" && kind == "message" => hash.update([0]),
            Some(Value::String(text)) => {
                hash.update([1]);
                field(&mut hash, normalize(text).as_bytes());
            }
            Some(Value::Array(parts)) if name == "content" => match text_parts(parts) {
                Some(text) => {
                    hash.update([1]);
                    field(&mut hash, normalize(&text).as_bytes());
                }
                None => {
                    hash.update([2]);
                    hash.update((parts.len() as u64).to_le_bytes());
                    for part in parts {
                        match text_part(part) {
                            Some(text) => {
                                hash.update([1]);
                                field(&mut hash, normalize(text).as_bytes());
                            }
                            None => {
                                hash.update([2]);
                                field(&mut hash, canonical(part).as_bytes());
                            }
                        }
                    }
                }
            },
            Some(value) => {
                hash.update([3]);
                field(&mut hash, canonical(value).as_bytes());
            }
        }
    }
    hash.finalize().into()
}

fn text_part(part: &Value) -> Option<&str> {
    match (part.get("type")?.as_str()?, part.get("text")?.as_str()) {
        ("text" | "input_text" | "output_text", Some(text)) => Some(text),
        _ => None,
    }
}

fn text_parts(parts: &[Value]) -> Option<String> {
    parts
        .iter()
        .map(text_part)
        .collect::<Option<Vec<_>>>()
        .map(|texts| texts.join("\n"))
}

fn canonical(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct StoredRow {
    pub id: String,
    pub charged: u64,
    pub model_tag: String,
    pub last_used: u64,
    pub busy: bool,
}

pub struct Budgets {
    pub model_bytes: u64,
    pub total_bytes: u64,
    pub sessions: usize,
}

pub fn plan_eviction(
    rows: &[StoredRow],
    model_tag: &str,
    reservation: u64,
    budgets: &Budgets,
) -> Option<Vec<usize>> {
    if reservation > budgets.model_bytes || budgets.sessions == 0 {
        return None;
    }
    let mut order = (0..rows.len())
        .filter(|index| !rows[*index].busy)
        .collect::<Vec<_>>();
    order.sort_by_key(|index| (rows[*index].last_used, rows[*index].id.clone()));
    let mut evicted = vec![false; rows.len()];
    let mut model_used = rows
        .iter()
        .filter(|row| row.model_tag == model_tag)
        .fold(0u64, |total, row| total.saturating_add(row.charged));
    let mut total_used = rows
        .iter()
        .fold(0u64, |total, row| total.saturating_add(row.charged));
    let mut count = rows.len();
    let mut plan = Vec::new();
    for global in [false, true] {
        for &index in &order {
            let fits = if global {
                total_used.saturating_add(reservation) <= budgets.total_bytes
                    && count < budgets.sessions
            } else {
                model_used.saturating_add(reservation) <= budgets.model_bytes
            };
            if fits {
                break;
            }
            if evicted[index] || (!global && rows[index].model_tag != model_tag) {
                continue;
            }
            evicted[index] = true;
            if rows[index].model_tag == model_tag {
                model_used = model_used.saturating_sub(rows[index].charged);
            }
            total_used = total_used.saturating_sub(rows[index].charged);
            count -= 1;
            plan.push(index);
        }
    }
    (model_used.saturating_add(reservation) <= budgets.model_bytes
        && total_used.saturating_add(reservation) <= budgets.total_bytes
        && count < budgets.sessions)
        .then_some(plan)
}

pub fn per_token_bound(shape: &crate::profiles::ModelShape) -> Option<u64> {
    crate::engines::models::kv_bytes(shape, 1)?.checked_mul(2)
}

/// The bytes to set aside for a state about to be saved. `shape_bound` is twice the model's
/// KV size per token, a ceiling for a model nothing was saved from yet. `observed` is the
/// bytes per token of the largest state the worker has saved; once known it replaces that
/// ceiling, so a long conversation is not charged twice its size and does not evict states
/// that fit beside it.
pub fn reservation(
    tokens: Option<u64>,
    context: Option<u64>,
    shape_bound: Option<u64>,
    learned: Option<u64>,
    observed: Option<u64>,
    overhead: u64,
    fallback: u64,
) -> u64 {
    let per_token = match (shape_bound, learned) {
        (Some(shape), Some(learned)) => Some(shape.max(learned.saturating_mul(5) / 4)),
        (Some(shape), None) => Some(match observed {
            Some(observed) => shape.min(observed.saturating_mul(5) / 4),
            None => shape,
        }),
        (None, Some(learned)) => Some(learned.saturating_mul(5) / 4),
        (None, None) => None,
    };
    match (tokens.or(context), per_token) {
        (Some(tokens), Some(per_token)) => tokens
            .saturating_mul(per_token)
            .saturating_add(overhead)
            .saturating_add(RESERVATION_HEADROOM_BYTES)
            .min(fallback),
        _ => fallback,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    pub principal: &'static str,
    pub credential_epoch: std::sync::Arc<str>,
    pub protocol: String,
}

pub struct SlotCandidate<'a> {
    pub holder: Option<(&'a Scope, &'a Key)>,
    pub dirty: bool,
    pub last_used: u64,
}

pub fn slot_preference(slots: &[SlotCandidate<'_>], request: Option<(&Scope, &Key)>) -> Vec<usize> {
    let similarity = |slot: &SlotCandidate<'_>| match (slot.holder, request) {
        (Some((held_scope, held)), Some((scope, key))) if held_scope == scope => {
            (held.common_units(key), held.text_blocks_shared(key))
        }
        _ => (0, 0),
    };
    let mut order = (0..slots.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let slot = &slots[*index];
        let (affinity, text) = similarity(slot);
        // A holder that matches only partway into a message is usually the same client's
        // superseded conversation: replacing it reuses what still matches and keeps other
        // clients' states resident.
        let class = if affinity > 0 {
            0
        } else if !slot.dirty && text > 0 {
            1
        } else if slot.holder.is_none() && !slot.dirty {
            2
        } else if !slot.dirty {
            3
        } else {
            4
        };
        (
            class,
            std::cmp::Reverse(affinity),
            std::cmp::Reverse(text),
            slot.last_used,
            *index,
        )
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_and_retained_sessions_are_bounded() {
        let mut settings = Settings::default();
        assert!(settings.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&settings).unwrap().get("retained"),
            None
        );
        settings.retention_days = Some(0);
        assert!(settings.validate().is_err());
        settings.retention_days = Some(7);
        settings.retained.insert("a".repeat(64));
        assert!(settings.validate().is_ok());
        settings.retained.insert("not-a-digest".into());
        assert!(settings.validate().is_err());
        let parsed: Settings =
            serde_json::from_value(serde_json::json!({"retention_days": 30})).unwrap();
        assert_eq!(parsed.retention_days, Some(30));
        assert!(parsed.retained.is_empty());
    }
    use serde_json::json;

    fn chat(messages: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"model":"m","messages":messages,"stream":true})).unwrap()
    }

    fn scope(principal: &'static str) -> Scope {
        Scope {
            principal,
            credential_epoch: "epoch".into(),
            protocol: protocol(Operation::Chat, KeyKind::Prefix, "m").unwrap(),
        }
    }

    #[test]
    fn prefix_fingerprints_are_stable_across_turns_and_ignore_transport_details() {
        let first = chat(json!([
            {"role":"system","content":"Be brief."},
            {"role":"user","content":"Hello\r\n"}
        ]));
        let next = serde_json::to_vec(&json!({"stream":false,"temperature":0.2,"messages":[
            {"content":"Be brief.","role":"system"},
            {"role":"user","content":[{"type":"text","text":"Hello"}]},
            {"role":"assistant","content":"Hi."},
            {"role":"user","content":"More?"}
        ],"model":"m"}))
        .unwrap();
        let first = Key::prefix(Operation::Chat, &first).unwrap();
        let next = Key::prefix(Operation::Chat, &next).unwrap();
        assert_eq!(first.units(), 2);
        assert_eq!(next.units(), 4);
        assert!(next.extends(&first));
        assert_eq!(next.candidates().nth(1).unwrap().1, first.saved_digest());
        assert_eq!(next.ancestors().count(), 3);
        let other = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"system","content":"Be verbose."},{"role":"user","content":"Hello"}])),
        )
        .unwrap();
        assert_eq!(next.common_units(&other), 0);
        let chat_key = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"user","content":"Hello"}])),
        )
        .unwrap();
        let responses = Key::prefix(
            Operation::Responses,
            &serde_json::to_vec(&json!({"model":"m","input":[{"role":"user","content":"Hello"}]}))
                .unwrap(),
        )
        .unwrap();
        assert_ne!(chat_key.saved_digest(), responses.saved_digest());
        assert!(!chat_key.saved_digest().contains("Hello") && chat_key.saved_digest().len() == 64);
    }

    #[test]
    fn shared_leading_messages_report_their_bytes_and_survive_a_lineage_round_trip() {
        let system = json!({"role":"system","content":"House rules. ".repeat(40)});
        let saved = Key::prefix(
            Operation::Chat,
            &chat(json!([system, {"role":"user","content":"Lights?"},{"role":"assistant","content":"On."}])),
        )
        .unwrap();
        let fresh = Key::prefix(
            Operation::Chat,
            &chat(json!([system, {"role":"user","content":"Heating?"}])),
        )
        .unwrap();
        let system_bytes = canonical(&system).len() as u64;
        assert_eq!(fresh.shared(&saved), (1, system_bytes));
        assert_eq!(saved.shared(&saved).0, 3);
        let restored = Key::from_record(&saved.lineage().to_record(false)).unwrap();
        assert_eq!(restored.shared(&saved), saved.shared(&saved));
        assert_eq!(fresh.shared(&restored), (1, system_bytes));
        let header = Key::header("thread", Operation::Chat, b"{}");
        assert_eq!(fresh.shared(&header), (0, 0));
        assert_eq!(fresh.shared_bytes(&header), 0);
        let mut broken = saved.to_record(false);
        broken.chain[0].replace_range(0..2, "zz");
        assert!(Key::from_record(&broken).is_none());
        broken = saved.to_record(false);
        broken.bytes.pop();
        assert!(Key::from_record(&broken).is_none());
        let long = Key::prefix(
            Operation::Chat,
            &chat(Value::Array(
                (0..LINEAGE_UNITS + 5)
                    .map(|index| json!({"role":"user","content":index.to_string()}))
                    .collect(),
            )),
        )
        .unwrap();
        assert_eq!(long.lineage().units(), LINEAGE_UNITS);
        assert!(long.extends(&long.lineage()));
    }

    #[test]
    fn a_sibling_replaces_another_only_when_the_shared_opening_is_most_of_both() {
        let ask = |system: &str, question: &str| {
            Key::prefix(
                Operation::Chat,
                &chat(
                    json!([{"role":"system","content":system},{"role":"user","content":question}]),
                ),
            )
            .unwrap()
        };
        let long = "Exposed devices: porch light, hallway sensor. ".repeat(100);
        let (first, second) = (ask(&long, "Porch on?"), ask(&long, "Garage open?"));
        assert!(second.replaces_sibling(&first.lineage()));
        assert!(first.replaces_sibling(&second.lineage()));

        let short = "Be brief and kind. ".repeat(20);
        assert!(short.len() < MINIMUM_SHARED_BYTES as usize);
        assert!(!ask(&short, "One?").replaces_sibling(&ask(&short, "Two?").lineage()));

        let medium = "House rules. ".repeat(200);
        let essay = "Compare these two long drafts line by line. ".repeat(20);
        let (brief, wordy) = (ask(&medium, "Lights?"), ask(&medium, &essay));
        assert!(brief.shared(&wordy).1 >= MINIMUM_SHARED_BYTES);
        assert!(!brief.replaces_sibling(&wordy.lineage()));
        assert!(!wordy.replaces_sibling(&brief.lineage()));

        assert!(
            !ask(&long, "Porch on?")
                .replaces_sibling(&ask(&long.replace("porch", "patio"), "Porch on?").lineage())
        );
        assert!(!first.replaces_sibling(&Key::header("thread", Operation::Chat, b"{}")));
        let huge = long.repeat(20);
        let many = Key::prefix(
            Operation::Chat,
            &chat(Value::Array(
                std::iter::once(json!({"role":"system","content":huge}))
                    .chain(
                        (0..LINEAGE_UNITS)
                            .map(|index| json!({"role":"user","content":index.to_string()})),
                    )
                    .collect(),
            )),
        )
        .unwrap();
        let one = ask(&huge, "Porch on?");
        assert!(many.replaces_sibling(&one.lineage()));
        assert!(one.replaces_sibling(&many.lineage()));
        let without_text = Key::from_record(&LineageRecord {
            text: String::new(),
            ..many.lineage().to_record(true)
        })
        .unwrap();
        assert!(!one.replaces_sibling(&without_text));

        assert!(!first.carries_reply() && !many.carries_reply());
        let turns = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"system","content":long},{"role":"user","content":"Porch on?"},{"role":"assistant","content":"On."},{"role":"user","content":"Off?"}])),
        )
        .unwrap();
        assert!(turns.carries_reply());
        let responses = |input: Value| {
            Key::prefix(
                Operation::Responses,
                &serde_json::to_vec(&json!({"model":"m","input":input})).unwrap(),
            )
            .unwrap()
            .carries_reply()
        };
        assert!(!responses(json!([{"role":"user","content":"Porch on?"}])));
        assert!(responses(
            json!([{"role":"user","content":"Porch on?"},{"type":"function_call_output","call_id":"1","output":"on"}])
        ));
        assert!(responses(
            json!([{"type":"message","role":"assistant","content":"On."},{"role":"user","content":"Off?"}])
        ));

        let record = first.lineage().to_record(true);
        assert_eq!(serde_json::to_value(&record).unwrap()["single"], true);
        assert!(
            serde_json::to_value(first.lineage().to_record(false))
                .unwrap()
                .get("single")
                .is_none()
        );
        let legacy: LineageRecord =
            serde_json::from_value(json!({"chain": record.chain, "bytes": record.bytes})).unwrap();
        assert!(!legacy.single);
    }

    #[test]
    fn completion_prompts_use_full_blocks_and_tool_content_changes_the_unit() {
        let short = serde_json::to_vec(&json!({"model":"m","prompt":"short"})).unwrap();
        assert!(Key::prefix(Operation::Completion, &short).is_none());
        let base = "x".repeat(PROMPT_BLOCK_BYTES * 2 + 7);
        let longer = format!("{base}{}", "y".repeat(PROMPT_BLOCK_BYTES));
        let first = Key::prefix(
            Operation::Completion,
            &serde_json::to_vec(&json!({"model":"m","prompt":base})).unwrap(),
        )
        .unwrap();
        let second = Key::prefix(
            Operation::Completion,
            &serde_json::to_vec(&json!({"model":"m","prompt":longer})).unwrap(),
        )
        .unwrap();
        assert_eq!(first.units(), 2);
        assert!(second.extends(&first));
        assert!(Key::prefix(Operation::Embeddings, &short).is_none());
        let call = |argument: &str| {
            Key::prefix(
                Operation::Chat,
                &chat(json!([{"role":"assistant","content":null,"tool_calls":[{"id":"1","type":"function","function":{"name":"f","arguments":argument}}]}])),
            )
            .unwrap()
        };
        assert_ne!(call("{}").saved_digest(), call("{\"a\":1}").saved_digest());
        assert!(Key::prefix(Operation::Chat, b"{\"model\":\"m\",\"messages\":[]}").is_none());
        assert!(Key::prefix(Operation::Chat, b"not json").is_none());
    }

    #[test]
    fn header_sessions_are_digests_and_differ_from_prefix_keys() {
        let header = |session: &str, body: &[u8]| Key::header(session, Operation::Chat, body);
        let one = header("thread-1", b"{}");
        assert_eq!(one, header("thread-1", b"{}"));
        assert_ne!(one, header("thread-2", b"{}"));
        assert_eq!(one.units(), 1);
        assert!(!one.saved_digest().contains("thread"));
        let prefix = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"user","content":"thread-1"}])),
        )
        .unwrap();
        assert_eq!(one.common_units(&prefix), 0);
        assert_eq!(one.shared_bytes(&prefix), 0);

        let rules = "House rule. ".repeat(200);
        let body = |question: &str| {
            chat(json!([{"role":"system","content":rules},{"role":"user","content":question}]))
        };
        let (first, second) = (
            header("thread-1", &body("One?")),
            header("thread-2", &body("Two?")),
        );
        assert_eq!(
            first.saved_digest(),
            header("thread-1", b"{}").saved_digest()
        );
        assert_eq!(
            first.shared_bytes(&header("thread-1", &body("Other?"))),
            u64::MAX
        );
        let shared = first.shared_bytes(&second);
        assert!(
            (2048..rules.len() as u64 + 64).contains(&shared),
            "{shared}"
        );
        assert_eq!(
            first.shared_bytes(&Key::prefix(Operation::Chat, &body("One?")).unwrap()),
            0
        );
        let record = first.lineage().to_record(false);
        assert!(record.header);
        let restored = Key::from_record(&record).unwrap();
        assert_eq!(restored.kind, KeyKind::Header);
        assert_eq!(second.shared_bytes(&restored), shared);
        assert_eq!(first.shared_bytes(&restored), u64::MAX);
    }

    fn ask(system: &str, question: &str) -> Key {
        Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"system","content":system},{"role":"user","content":question}])),
        )
        .unwrap()
    }

    #[test]
    fn shared_text_is_measured_to_where_two_requests_first_differ() {
        let document = (0..400)
            .map(|line| format!("Record {line}: sensor {} reads {}.\n", line * 7, line * 13))
            .collect::<String>();
        let length = document.len() as u64;
        let dated = |day: u32| ask(&format!("{document}Today is day {day}."), "Lights?");
        let (first, second) = (dated(1), dated(2));
        assert_eq!(first.common_units(&second), 0);
        assert_eq!(first.shared(&second), (0, 0));
        let shared = first.shared_bytes(&second);
        assert!(
            shared <= length + 16 && shared + 256 > length,
            "{shared} of {length}"
        );
        assert_eq!(second.shared_bytes(&first), shared);
        assert_eq!(
            first.shared_bytes(&first.lineage()),
            first.shared_bytes(&first)
        );

        let early = |day: u32| ask(&format!("Today is day {day}.\n{document}"), "Lights?");
        assert_eq!(early(1).shared_bytes(&early(2)), 0);
        let one_byte_in = |index: usize| {
            let mut text = document.clone().into_bytes();
            text[index] ^= 1;
            ask(&String::from_utf8(text).unwrap(), "Lights?")
        };
        let whole = ask(&document, "Lights?");
        for index in [0, 255, 256, 1000, 5000] {
            let shared = whole.shared_bytes(&one_byte_in(index));
            assert!(shared <= index as u64 + 16, "{index}: {shared}");
            assert!(shared + 512 > index as u64, "{index}: {shared}");
        }
        let swapped = ask(&document.replacen("Record 3:", "Record 4:", 1), "Lights?");
        assert!(whole.shared_bytes(&swapped) < 256);
        let longer = ask(&format!("{document}{document}"), "Lights?");
        let shared = whole.shared_bytes(&longer);
        assert!(shared <= length + 16 && shared + 256 > length, "{shared}");
        let reordered = Key::prefix(
            Operation::Chat,
            &chat(
                json!([{"role":"user","content":"Lights?"},{"role":"system","content":document}]),
            ),
        )
        .unwrap();
        assert_eq!(whole.shared_bytes(&reordered), 0);
        let assistant = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"assistant","content":document},{"role":"user","content":"Lights?"}])),
        )
        .unwrap();
        assert_eq!(whole.shared_bytes(&assistant), 0);

        let after_system = |question: &str| {
            ask(
                "Answer from the document.",
                &format!("{document}Question: {question}"),
            )
        };
        let (one, two) = (
            after_system("Which sensor?"),
            after_system("How many records?"),
        );
        assert_eq!(one.common_units(&two), 1);
        assert!(one.shared(&two).1 < 128);
        assert!(one.shared_bytes(&two) + 256 > length);
        let parts = Key::prefix(
            Operation::Chat,
            &chat(json!([
                {"role":"system","content":"Answer from the document."},
                {"role":"user","content":[{"type":"text","text":format!("{document}Question: Which sensor?")}]}
            ])),
        )
        .unwrap();
        assert_eq!(parts.saved_digest(), one.saved_digest());
        assert_eq!(parts.shared_bytes(&two), one.shared_bytes(&two));

        let responses = |question: &str| {
            Key::prefix(
                Operation::Responses,
                &serde_json::to_vec(&json!({"model":"m","instructions":"Answer from the document.","input":[{"role":"user","content":format!("{document}Question: {question}")}]}))
                    .unwrap(),
            )
            .unwrap()
        };
        assert!(responses("Which sensor?").shared_bytes(&responses("How many?")) + 256 > length);
        let completion = |tail: &str| {
            Key::prefix(
                Operation::Completion,
                &serde_json::to_vec(&json!({"model":"m","prompt":format!("{document}{tail}")}))
                    .unwrap(),
            )
            .unwrap()
        };
        assert!(completion("Summary:").shared_bytes(&completion("Outline:")) + 512 > length);

        let huge =
            "x".repeat((FINE_BLOCKS * FINE_BLOCK_BYTES + COARSE_BLOCKS * COARSE_BLOCK_BYTES) * 2);
        let capped = ask(&huge, "q");
        assert_eq!(capped.text.len(), TEXT_BLOCKS);
        assert_eq!(text_bytes(TEXT_BLOCKS), 1024 * 1024);
        assert_eq!(text_bytes(FINE_BLOCKS + 1), 65536 + 1024);
        let record = capped.lineage().to_record(false);
        assert_eq!(record.text.len(), TEXT_BLOCKS * 16);
        assert_eq!(Key::from_record(&record).unwrap().text, capped.text);
        for broken in ["abc", "zz00000000000000"] {
            let mut record = first.lineage().to_record(false);
            record.text = broken.into();
            assert!(Key::from_record(&record).is_none(), "{broken}");
        }
        let mut record = capped.lineage().to_record(false);
        record.text.push_str("0000000000000000");
        assert!(Key::from_record(&record).is_none());
        let legacy: LineageRecord = serde_json::from_value(
            json!({"chain": first.lineage().to_record(false).chain, "bytes": first.lineage().to_record(false).bytes}),
        )
        .unwrap();
        let legacy = Key::from_record(&legacy).unwrap();
        assert_eq!(second.shared_bytes(&legacy), 0);
        assert_eq!(first.shared_bytes(&legacy), first.shared(&first).1);
    }

    #[test]
    fn one_off_requests_that_differ_inside_a_message_replace_each_other() {
        let rules = "Exposed devices: porch light, hallway sensor. ".repeat(100);
        let dated =
            |day: u32, question: &str| ask(&format!("{rules}Today is day {day}."), question);
        assert_eq!(dated(1, "Porch on?").common_units(&dated(2, "Garage?")), 0);
        assert!(dated(2, "Garage?").replaces_sibling(&dated(1, "Porch on?").lineage()));
        let early = |day: u32| ask(&format!("Today is day {day}. {rules}"), "Porch on?");
        assert!(!early(2).replaces_sibling(&early(1).lineage()));
        let half = |tail: &str| ask(&format!("{rules}{}", tail.repeat(120)), "Porch on?");
        assert!(half("Cats sleep. ").shared_bytes(&half("Dogs bark. ")) >= MINIMUM_SHARED_BYTES);
        assert!(!half("Cats sleep. ").replaces_sibling(&half("Dogs bark. ").lineage()));
        let huge = "y".repeat(3 * 1024 * 1024);
        let capped = |question: &str| ask("Rules.", &format!("{huge}{question}"));
        assert!(!capped("One?").replaces_sibling(&capped("Two?").lineage()));
    }

    #[test]
    fn a_restore_must_repay_its_cost_in_text_the_engine_need_not_read_again() {
        const MIB: u64 = 1024 * 1024;
        let unknown = Rates::default();
        assert!(!worth_restoring(MINIMUM_SHARED_BYTES - 1, 0, unknown));
        assert!(worth_restoring(MINIMUM_SHARED_BYTES, 0, unknown));
        assert!(worth_restoring(8 * 1024, 256 * MIB, unknown));
        assert!(!worth_restoring(2 * 1024, 256 * MIB, unknown));
        let slow_engine = Rates {
            prefill_tokens_per_second: Some(200.0),
            restore_bytes_per_second: Some(1024.0 * MIB as f64),
        };
        assert!(worth_restoring(8 * 1024, 2304 * MIB, slow_engine));
        assert!(!worth_restoring(2 * 1024, 2304 * MIB, slow_engine));
        let fast_engine = Rates {
            prefill_tokens_per_second: Some(5000.0),
            restore_bytes_per_second: Some(1024.0 * MIB as f64),
        };
        assert!(!worth_restoring(8 * 1024, 2304 * MIB, fast_engine));
        assert!(worth_restoring(160 * 1024, 2304 * MIB, fast_engine));
        assert!(!worth_restoring(MINIMUM_SHARED_BYTES, MIB, fast_engine));
        assert!(worth_restoring(4 * 1024, MIB, fast_engine));
        let broken = Rates {
            prefill_tokens_per_second: Some(0.0),
            restore_bytes_per_second: Some(f64::NAN),
        };
        assert_eq!(
            worth_restoring(8 * 1024, 256 * MIB, broken),
            worth_restoring(8 * 1024, 256 * MIB, unknown)
        );
    }

    #[test]
    fn protocol_tags_round_trip_and_reject_foreign_values() {
        let value = protocol(Operation::Responses, KeyKind::Header, "my model").unwrap();
        assert!(value.len() <= 64);
        assert_eq!(
            parse_protocol(&value),
            Some(ProtocolTag {
                operation: "responses",
                kind: KeyKind::Header,
                model_tag: &model_tag("my model"),
            })
        );
        assert!(protocol(Operation::Embeddings, KeyKind::Prefix, "m").is_none());
        for foreign in [
            "openai.completions",
            "auto.chat.p",
            "auto.chat.x.0123456789abcdef",
            "auto.chat.p.0123456789abcdef.extra",
            "auto.embeddings.p.0123456789abcdef",
        ] {
            assert!(parse_protocol(foreign).is_none(), "{foreign}");
        }
    }

    #[test]
    fn request_bodies_keep_unknown_fields_when_slot_routing_is_added() {
        let body = br#"{"model":"m","messages":[],"custom":{"x":1}}"#;
        let routed: Value = serde_json::from_slice(&with_slot(body, 3).unwrap()).unwrap();
        assert_eq!(routed["id_slot"], 3);
        assert_eq!(routed["custom"]["x"], 1);
        assert!(with_slot(b"[]", 0).is_none());
        assert!(prompt_cache_disabled(br#"{"cache_prompt":false}"#));
        assert!(!prompt_cache_disabled(br#"{"cache_prompt":true}"#));
        assert!(!prompt_cache_disabled(br#"{"model":"m"}"#));
    }

    fn row(id: &str, charged: u64, model: &str, last_used: u64) -> StoredRow {
        StoredRow {
            id: id.into(),
            charged,
            model_tag: model.into(),
            last_used,
            busy: false,
        }
    }

    #[test]
    fn eviction_removes_least_recent_rows_for_the_model_then_globally() {
        let rows = vec![
            row("a", 40, "m", 3),
            row("b", 40, "m", 1),
            row("c", 40, "n", 0),
            row("d", 40, "m", 2),
        ];
        let budgets = Budgets {
            model_bytes: 100,
            total_bytes: 1000,
            sessions: 100,
        };
        let plan = plan_eviction(&rows, "m", 40, &budgets).unwrap();
        assert_eq!(
            plan.iter()
                .map(|index| rows[*index].id.as_str())
                .collect::<Vec<_>>(),
            ["b", "d"]
        );
        let global = Budgets {
            model_bytes: 1000,
            total_bytes: 120,
            sessions: 100,
        };
        let plan = plan_eviction(&rows, "m", 40, &global).unwrap();
        assert_eq!(
            plan.iter()
                .map(|index| rows[*index].id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b"]
        );
        let count = Budgets {
            model_bytes: 1000,
            total_bytes: 1000,
            sessions: 2,
        };
        assert_eq!(plan_eviction(&rows, "m", 1, &count).unwrap().len(), 3);
        assert!(plan_eviction(&rows, "m", 101, &budgets).is_none());
        let mut busy = rows.clone();
        for row in &mut busy {
            row.busy = true;
        }
        assert!(plan_eviction(&busy, "m", 40, &budgets).is_none());
        assert_eq!(
            plan_eviction(&[], "m", 40, &budgets).unwrap(),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn reservations_use_shape_or_learned_bounds_and_cap_at_the_budget() {
        let fallback = 1 << 30;
        assert_eq!(
            reservation(Some(10), None, None, None, None, 0, fallback),
            fallback
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), None, None, 0, fallback),
            1000 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), None, None, 7, fallback),
            1007 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(None, Some(20), None, Some(100), None, 0, fallback),
            2500 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), Some(400), None, 0, fallback),
            5000 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(u64::MAX), None, Some(2), None, None, 0, fallback),
            fallback
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), None, Some(52), 0, fallback),
            650 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), None, Some(400), 0, fallback),
            1000 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(10), None, None, None, Some(52), 0, fallback),
            fallback
        );
        let shape = crate::profiles::ModelShape {
            block_count: Some(28),
            embedding_length: Some(1024),
            head_count: Some(16),
            head_count_kv: Some(8),
            key_length: Some(128),
            value_length: Some(128),
            ..Default::default()
        };
        assert_eq!(per_token_bound(&shape), Some(28 * 8 * 256 * 2 * 2));
    }

    #[test]
    fn slots_prefer_the_holder_then_a_partial_match_then_empty_then_least_recent_clean_state() {
        let me = scope("client");
        let other = scope("admin");
        let mine = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"user","content":"a"},{"role":"assistant","content":"b"}])),
        )
        .unwrap();
        let request = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"user","content":"a"},{"role":"assistant","content":"b"},{"role":"user","content":"c"}])),
        )
        .unwrap();
        let slots = [
            SlotCandidate {
                holder: Some((&other, &mine)),
                dirty: false,
                last_used: 1,
            },
            SlotCandidate {
                holder: None,
                dirty: false,
                last_used: 9,
            },
            SlotCandidate {
                holder: Some((&me, &mine)),
                dirty: true,
                last_used: 5,
            },
            SlotCandidate {
                holder: Some((&me, &request)),
                dirty: true,
                last_used: 0,
            },
        ];
        assert_eq!(slot_preference(&slots, Some((&me, &request))), [3, 2, 1, 0]);
        assert_eq!(slot_preference(&slots, None), [1, 0, 3, 2]);
        let rules = |live: &str| {
            Key::prefix(
                Operation::Chat,
                &chat(json!([{"role":"system","content":format!("{}{live}", "Static rule. ".repeat(60))},{"role":"user","content":"q"}])),
            )
            .unwrap()
        };
        let (old, new, unrelated) = (rules("Free 3 GiB."), rules("Free 4 GiB."), Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"system","content":"Other client. ".repeat(60)},{"role":"user","content":"q"}])),
        )
        .unwrap());
        assert_eq!(old.common_units(&new), 0);
        assert!(old.text_blocks_shared(&new) >= 2);
        assert_eq!(old.text_blocks_shared(&unrelated), 0);
        let full = [
            SlotCandidate {
                holder: Some((&me, &unrelated)),
                dirty: false,
                last_used: 1,
            },
            SlotCandidate {
                holder: Some((&me, &old)),
                dirty: false,
                last_used: 9,
            },
        ];
        assert_eq!(slot_preference(&full, Some((&me, &new))), [1, 0]);
        let with_empty = [
            SlotCandidate {
                holder: None,
                dirty: false,
                last_used: 0,
            },
            SlotCandidate {
                holder: Some((&me, &old)),
                dirty: false,
                last_used: 9,
            },
        ];
        assert_eq!(slot_preference(&with_empty, Some((&me, &new))), [1, 0]);
        assert_eq!(
            slot_preference(&with_empty, Some((&me, &unrelated))),
            [0, 1]
        );
        assert_eq!(slot_preference(&full, Some((&other, &new))), [0, 1]);
    }

    #[test]
    fn settings_validate_bounds_and_overrides() {
        let mut settings = Settings::default();
        assert!(settings.is_default());
        settings.validate().unwrap();
        settings.models.insert("chat".into(), Mode::Memory);
        assert_eq!(settings.mode_for("chat"), Mode::Memory);
        assert_eq!(settings.mode_for("other"), Mode::Persistent);
        settings.model_budget_bytes = settings.budget_bytes + 1;
        assert!(settings.validate().is_err());
        let parsed: Settings = serde_json::from_value(json!({"mode":"off"})).unwrap();
        assert_eq!(parsed.mode, Mode::Off);
        assert_eq!(parsed.budget_bytes, Settings::default().budget_bytes);
        assert!(serde_json::from_value::<Settings>(json!({"unknown":1})).is_err());
        assert!(
            serde_json::to_value(Settings::default())
                .unwrap()
                .get("models")
                .is_none()
        );
    }
}
