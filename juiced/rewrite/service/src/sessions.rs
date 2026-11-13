use crate::config::Operation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAXIMUM_UNITS: usize = 4096;
pub const PROMPT_BLOCK_BYTES: usize = 512;
pub const SESSION_HEADERS: [&str; 3] = [
    "x-juiced-session",
    "x-juiced-session-id",
    "x-juiced-thread-id",
];
pub const PROTOCOL_PREFIX: &str = "auto.";
pub const LINEAGE_UNITS: usize = 64;
const LEAD_BLOCK_BYTES: usize = 256;
const LEAD_BLOCKS: usize = 256;
// Below about 500 tokens, re-reading a whole saved slot costs more than prefilling the shared part.
pub const MINIMUM_SHARED_BYTES: u64 = 2048;
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
        Ok(())
    }

    pub fn mode_for(&self, model: &str) -> Mode {
        self.models.get(model).copied().unwrap_or(self.mode)
    }
}

pub type UnitDigest = [u8; 32];

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
    lead: Vec<UnitDigest>,
}

impl Key {
    pub fn header(session: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"juiced-session-header-v1\0");
        hash.update(session.as_bytes());
        Self {
            kind: KeyKind::Header,
            chain: vec![hash.finalize().into()],
            bytes: vec![0],
            lead: Vec::new(),
        }
    }

    pub fn prefix(operation: Operation, body: &[u8]) -> Option<Self> {
        let (units, lead) = match operation {
            Operation::Chat => chat_units(body)?,
            Operation::Completion => completion_units(body)?,
            Operation::Responses => responses_units(body)?,
            _ => return None,
        };
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
            lead: lead_blocks(&lead),
        })
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

    /// Leading blocks of the first unit shared with `other`, for requests whose first message
    /// changed partway through, such as a system prompt that embeds live figures.
    pub fn lead_blocks_shared(&self, other: &Self) -> usize {
        if self.kind != other.kind {
            return 0;
        }
        self.lead
            .iter()
            .zip(&other.lead)
            .take_while(|(left, right)| left == right)
            .count()
    }

    pub fn lineage(&self) -> Self {
        let units = self.chain.len().min(LINEAGE_UNITS);
        Self {
            kind: self.kind,
            chain: self.chain[..units].to_vec(),
            bytes: self.bytes[..units].to_vec(),
            lead: Vec::new(),
        }
    }

    pub fn to_record(&self) -> LineageRecord {
        LineageRecord {
            chain: self.chain.iter().map(hex).collect(),
            bytes: self.bytes.clone(),
        }
    }

    pub fn from_record(record: &LineageRecord) -> Option<Self> {
        if record.chain.is_empty()
            || record.chain.len() > LINEAGE_UNITS
            || record.chain.len() != record.bytes.len()
        {
            return None;
        }
        let chain = record
            .chain
            .iter()
            .map(|digest| unhex(digest))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            kind: KeyKind::Prefix,
            chain,
            bytes: record.bytes.clone(),
            lead: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LineageRecord {
    pub chain: Vec<String>,
    pub bytes: Vec<u64>,
}

fn unhex(text: &str) -> Option<UnitDigest> {
    if text.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
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

fn message_unit(message: &Value) -> Unit {
    (message_digest(message), canonical(message).len() as u64)
}

fn lead_blocks(text: &[u8]) -> Vec<UnitDigest> {
    let mut previous: UnitDigest = Sha256::digest(b"juiced-lead-v1\0").into();
    text.as_chunks::<LEAD_BLOCK_BYTES>()
        .0
        .iter()
        .take(LEAD_BLOCKS)
        .map(|block| {
            previous = Sha256::new()
                .chain_update(previous)
                .chain_update(block)
                .finalize()
                .into();
            previous
        })
        .collect()
}

fn chat_units(body: &[u8]) -> Option<(Vec<Unit>, Vec<u8>)> {
    #[derive(Deserialize)]
    struct Chat {
        messages: Vec<Value>,
    }
    let chat: Chat = serde_json::from_slice(body).ok()?;
    let lead = chat.messages.first().map(canonical).unwrap_or_default();
    Some((
        chat.messages.iter().map(message_unit).collect(),
        lead.into_bytes(),
    ))
}

fn responses_units(body: &[u8]) -> Option<(Vec<Unit>, Vec<u8>)> {
    #[derive(Deserialize)]
    struct Responses {
        input: Value,
        instructions: Option<String>,
    }
    let request: Responses = serde_json::from_slice(body).ok()?;
    let mut units = Vec::new();
    let lead = match (&request.instructions, &request.input) {
        (Some(instructions), _) => instructions.clone(),
        (None, Value::String(text)) => text.clone(),
        (None, Value::Array(items)) => items.first().map(canonical).unwrap_or_default(),
        (None, _) => String::new(),
    };
    if let Some(instructions) = &request.instructions {
        units.push((
            text_digest(b"instructions", instructions),
            instructions.len() as u64,
        ));
    }
    match &request.input {
        Value::String(text) => units.extend(text_blocks(text)),
        Value::Array(items) => units.extend(items.iter().map(message_unit)),
        _ => return None,
    }
    Some((units, lead.into_bytes()))
}

fn completion_units(body: &[u8]) -> Option<(Vec<Unit>, Vec<u8>)> {
    #[derive(Deserialize)]
    struct Completion {
        prompt: Value,
    }
    let request: Completion = serde_json::from_slice(body).ok()?;
    match &request.prompt {
        Value::String(text) => Some((text_blocks(text).collect(), Vec::new())),
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

pub fn reservation(
    tokens: Option<u64>,
    context: Option<u64>,
    shape_bound: Option<u64>,
    learned: Option<u64>,
    overhead: u64,
    fallback: u64,
) -> u64 {
    let per_token = match (shape_bound, learned) {
        (Some(shape), Some(learned)) => Some(shape.max(learned.saturating_mul(5) / 4)),
        (Some(shape), None) => Some(shape),
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
            (held.common_units(key), held.lead_blocks_shared(key))
        }
        _ => (0, 0),
    };
    let mut order = (0..slots.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let slot = &slots[*index];
        let (affinity, lead) = similarity(slot);
        // A holder whose first message matches only partway is usually the same client's
        // superseded conversation: replacing it reuses what still matches and keeps other
        // clients' states resident.
        let class = if affinity > 0 {
            0
        } else if !slot.dirty && lead > 0 {
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
            std::cmp::Reverse(lead),
            slot.last_used,
            *index,
        )
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let restored = Key::from_record(&saved.lineage().to_record()).unwrap();
        assert_eq!(restored.shared(&saved), saved.shared(&saved));
        assert_eq!(fresh.shared(&restored), (1, system_bytes));
        let header = Key::header("thread");
        assert_eq!(fresh.shared(&header), (0, 0));
        let mut broken = saved.to_record();
        broken.chain[0].replace_range(0..2, "zz");
        assert!(Key::from_record(&broken).is_none());
        broken = saved.to_record();
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
        let one = Key::header("thread-1");
        assert_eq!(one, Key::header("thread-1"));
        assert_ne!(one, Key::header("thread-2"));
        assert_eq!(one.units(), 1);
        assert!(!one.saved_digest().contains("thread"));
        let prefix = Key::prefix(
            Operation::Chat,
            &chat(json!([{"role":"user","content":"thread-1"}])),
        )
        .unwrap();
        assert_eq!(one.common_units(&prefix), 0);
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
            reservation(Some(10), None, None, None, 0, fallback),
            fallback
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), None, 0, fallback),
            1000 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), None, 7, fallback),
            1007 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(None, Some(20), None, Some(100), 0, fallback),
            2500 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(10), None, Some(100), Some(400), 0, fallback),
            5000 + RESERVATION_HEADROOM_BYTES
        );
        assert_eq!(
            reservation(Some(u64::MAX), None, Some(2), None, 0, fallback),
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
        assert!(old.lead_blocks_shared(&new) >= 2);
        assert_eq!(old.lead_blocks_shared(&unrelated), 0);
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
