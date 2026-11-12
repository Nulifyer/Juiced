#[path = "launch_plan.rs"]
mod launch_plan;
pub(crate) use launch_plan::{
    CapturedEnvironment, LaunchPlan, VerifiedLaunchBindings, phoneme_workspace,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fmt,
    io::{BufReader, Read, Seek, SeekFrom},
};

pub const MAX_PARALLEL_SLOTS: u32 = 4096;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchOptions {
    pub context_tokens: Option<u32>,
    pub threads: Option<u32>,
    pub gpu_layers: Option<u32>,
    pub parallel_slots: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLaunch {
    pub args: Vec<String>,
    pub applied: LaunchOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

const FLAGS: [&str; 4] = ["--ctx-size", "--threads", "--gpu-layers", "--parallel"];
pub(crate) fn option_field(flag: &str) -> Option<usize> {
    match flag {
        "--ctx-size" | "-c" => Some(0),
        "--threads" | "-t" => Some(1),
        "--gpu-layers" | "--n-gpu-layers" | "-ngl" => Some(2),
        "--parallel" | "-np" => Some(3),
        _ => None,
    }
}
impl LaunchOptions {
    pub fn validate(&self) -> Result<(), Error> {
        if self
            .context_tokens
            .is_some_and(|value| value == 0 || value > i32::MAX as u32)
            || self
                .threads
                .is_some_and(|value| value == 0 || value > 65536)
            || self.gpu_layers.is_some_and(|value| value > i32::MAX as u32)
            || self
                .parallel_slots
                .is_some_and(|value| value == 0 || value > MAX_PARALLEL_SLOTS)
        {
            return Err(Error::new(
                "invalid_launch_options",
                "Context must be 1 to 2147483647 tokens, threads 1 to 65536, GPU layers 0 to 2147483647, and parallel slots 1 to 4096.",
            ));
        }
        Ok(())
    }

    fn values(&self) -> [Option<u32>; 4] {
        [
            self.context_tokens,
            self.threads,
            self.gpu_layers,
            self.parallel_slots,
        ]
    }
    fn from_values(values: [Option<u32>; 4]) -> Self {
        Self {
            context_tokens: values[0],
            threads: values[1],
            gpu_layers: values[2],
            parallel_slots: values[3],
        }
    }

    pub fn resolve(&self, args: &[String]) -> Result<ResolvedLaunch, Error> {
        self.validate()?;
        let overrides = self.values();
        let mut applied = [None; 4];
        let mut seen: [Option<String>; 4] = std::array::from_fn(|_| None);
        let mut emitted = [false; 4];
        let mut resolved = Vec::with_capacity(args.len() + 8);
        let mut position = 0;
        while position < args.len() {
            if args[position] == "--" {
                break;
            }
            let (flag, inline) = args[position]
                .split_once('=')
                .map_or((args[position].as_str(), None), |(flag, value)| {
                    (flag, Some(value))
                });
            let Some(index) = option_field(flag) else {
                resolved.push(args[position].clone());
                position += 1;
                continue;
            };
            let value = inline
                .or_else(|| args.get(position + 1).map(String::as_str))
                .ok_or_else(|| {
                    Error::new(
                        "invalid_launch_arguments",
                        format!("{flag} requires a value."),
                    )
                })?;
            let (number, normalized) = legacy_value(index, value)?;
            if seen[index]
                .as_ref()
                .is_some_and(|previous| previous != &normalized)
            {
                return Err(Error::new(
                    "conflicting_launch_arguments",
                    format!("Conflicting values were supplied for {}.", FLAGS[index]),
                ));
            }
            seen[index] = Some(normalized);
            applied[index] = number;
            let consumed = if inline.is_some() { 1 } else { 2 };
            if let Some(value) = overrides[index] {
                if !emitted[index] {
                    resolved.push(FLAGS[index].to_owned());
                    resolved.push(value.to_string());
                    emitted[index] = true;
                }
                applied[index] = Some(value);
            } else {
                resolved.extend_from_slice(&args[position..position + consumed]);
            }
            position += consumed;
        }
        for (index, value) in overrides.into_iter().enumerate() {
            if let Some(value) = value {
                if !emitted[index] {
                    resolved.push(FLAGS[index].to_owned());
                    resolved.push(value.to_string());
                }
                applied[index] = Some(value);
            }
        }
        resolved.extend_from_slice(&args[position..]);
        let applied = Self::from_values(applied);
        applied.validate()?;
        Ok(ResolvedLaunch {
            args: resolved,
            applied,
        })
    }
}

fn legacy_value(index: usize, value: &str) -> Result<(Option<u32>, String), Error> {
    let auto = match index {
        0 => value == "0",
        1 => value.parse::<i32>().is_ok_and(|value| value <= 0),
        2 => matches!(value, "auto" | "all" | "-1" | "-2"),
        3 => value == "-1",
        _ => false,
    };
    if auto {
        return Ok((None, value.to_owned()));
    }
    let number = value.parse::<u32>().map_err(|_| {
        Error::new(
            "invalid_launch_arguments",
            format!(
                "{} requires a valid integer or its supported native sentinel.",
                FLAGS[index]
            ),
        )
    })?;
    if number == 0 && index != 2 {
        return Err(Error::new(
            "invalid_launch_arguments",
            format!("{} requires a positive value.", FLAGS[index]),
        ));
    }
    Ok((Some(number), number.to_string()))
}

const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_SLOTS: usize = MAX_PARALLEL_SLOTS as usize;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PropsObservation {
    pub source: &'static str,
    pub per_request_tokens: Option<u64>,
    pub slots: Option<u32>,
    pub engine_build: Option<String>,
}
impl PropsObservation {
    pub fn validate_capacity(&self, slots: &[SlotObservation], declared: u32) -> Result<(), Error> {
        self.validate_slots(slots)?;
        if declared == 0 || declared as usize > MAX_SLOTS || declared as usize != slots.len() {
            return Err(Error::new(
                "slot_capacity_mismatch",
                "The observed physical slot inventory disagrees with the declared launch profile.",
            ));
        }
        Ok(())
    }

    pub fn validate_slots(&self, slots: &[SlotObservation]) -> Result<(), Error> {
        if self
            .slots
            .is_some_and(|count| count as usize != slots.len())
        {
            return Err(Error::new(
                "slot_inventory_mismatch",
                "The observed /props slot count disagrees with the /slots inventory.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SlotObservation {
    pub id: u32,
    pub context_tokens: Option<u64>,
    pub is_processing: Option<bool>,
}

fn observation_json(bytes: &[u8]) -> Result<&serde_json::value::RawValue, Error> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err(Error::new(
            "observation_limit",
            "The worker observation exceeds 1 MiB.",
        ));
    }
    serde_json::from_slice(bytes).map_err(|_| {
        Error::new(
            "invalid_observation",
            "The worker observation is not valid JSON.",
        )
    })
}
fn observed_positive(
    value: Option<&serde_json::value::RawValue>,
    maximum: u64,
) -> Result<Option<u64>, Error> {
    let invalid = || {
        Error::new(
            "invalid_observation",
            "Observed capacity must be a bounded positive integer, zero, auto, or unknown.",
        )
    };
    let value = value
        .map(|raw| {
            if raw.get().len() > 26 {
                return Err(invalid());
            }
            serde_json::from_str::<Value>(raw.get()).map_err(|_| invalid())
        })
        .transpose()?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) if value == "auto" || value.as_i64().is_some_and(|number| number <= 0) => {
            Ok(None)
        }
        Some(value) => value
            .as_u64()
            .filter(|number| *number <= maximum)
            .map(|number| (number > 0).then_some(number))
            .ok_or_else(invalid),
    }
}

#[derive(Deserialize)]
struct PropsWire<'a> {
    #[serde(borrow)]
    default_generation_settings: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    total_slots: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    build_info: Option<&'a serde_json::value::RawValue>,
}
#[derive(Deserialize)]
struct SettingsWire<'a> {
    #[serde(borrow)]
    n_ctx: Option<&'a serde_json::value::RawValue>,
}
#[derive(Deserialize)]
struct SlotWire<'a> {
    #[serde(borrow)]
    id: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    n_ctx: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    is_processing: Option<&'a serde_json::value::RawValue>,
}

struct SlotValues<'a>(Vec<&'a serde_json::value::RawValue>);
impl<'de> Deserialize<'de> for SlotValues<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SlotsVisitor;
        impl<'de> serde::de::Visitor<'de> for SlotsVisitor {
            type Value = SlotValues<'de>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one to 4096 slot objects")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = input.next_element::<&serde_json::value::RawValue>()? {
                    if values.len() == MAX_SLOTS {
                        return Err(serde::de::Error::custom("Slot count exceeds limit"));
                    }
                    values.push(value);
                }
                if values.is_empty() {
                    return Err(serde::de::Error::custom("Empty slot inventory"));
                }
                Ok(SlotValues(values))
            }
        }
        deserializer.deserialize_seq(SlotsVisitor)
    }
}

pub fn parse_props(bytes: &[u8]) -> Result<PropsObservation, Error> {
    let raw = observation_json(bytes)?;
    let props: PropsWire<'_> = serde_json::from_str(raw.get()).map_err(|_| {
        Error::new(
            "invalid_observation",
            "The /props observation must be an object with unique fields.",
        )
    })?;
    let settings = props
        .default_generation_settings
        .map(|raw| serde_json::from_str::<SettingsWire<'_>>(raw.get()))
        .transpose()
        .map_err(|_| {
            Error::new(
                "invalid_observation",
                "Default generation settings must be an object.",
            )
        })?;
    let per_request_tokens =
        observed_positive(settings.and_then(|value| value.n_ctx), i64::MAX as u64)?;
    let slots = observed_positive(props.total_slots, MAX_SLOTS as u64)?.map(|value| value as u32);
    let engine_build = props
        .build_info
        .filter(|raw| raw.get().len() <= 6 * 256 + 2)
        .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok())
        .filter(|value| value.len() <= 256 && !value.chars().any(char::is_control));
    Ok(PropsObservation {
        source: "llamacpp:/props",
        per_request_tokens,
        slots,
        engine_build,
    })
}

pub fn parse_slots(bytes: &[u8]) -> Result<Vec<SlotObservation>, Error> {
    let input = observation_json(bytes)?;
    let entries: SlotValues<'_> = serde_json::from_str(input.get()).map_err(|_| {
        Error::new(
            "invalid_slot_inventory",
            "A /slots inventory requires 1 to 4096 slot objects.",
        )
    })?;
    let mut ids = HashSet::with_capacity(entries.0.len());
    let mut slots = Vec::with_capacity(entries.0.len());
    for raw in entries.0 {
        let entry: SlotWire<'_> = serde_json::from_str(raw.get()).map_err(|_| {
            Error::new(
                "invalid_slot_inventory",
                "Each slot must be an object with unique fields.",
            )
        })?;
        let id = entry
            .id
            .filter(|raw| raw.get().len() <= 10)
            .and_then(|raw| serde_json::from_str::<u32>(raw.get()).ok())
            .filter(|id| *id <= i32::MAX as u32)
            .ok_or_else(|| {
                Error::new(
                    "invalid_slot_inventory",
                    "Slot IDs must be unique nonnegative 32-bit integers.",
                )
            })?;
        if !ids.insert(id) {
            return Err(Error::new(
                "invalid_slot_inventory",
                "Slot IDs must be unique.",
            ));
        }
        let context_tokens = observed_positive(entry.n_ctx, i64::MAX as u64)?;
        let is_processing = entry
            .is_processing
            .map(|raw| serde_json::from_str::<Option<bool>>(raw.get()))
            .transpose()
            .map_err(|_| {
                Error::new(
                    "invalid_slot_inventory",
                    "Observed processing state must be boolean or unknown.",
                )
            })?
            .flatten();
        slots.push(SlotObservation {
            id,
            context_tokens,
            is_processing,
        });
    }
    Ok(slots)
}

const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_METADATA_ENTRIES: u64 = 4096;
const MAX_STRING_BYTES: u64 = 1024 * 1024;
const MAX_ARRAY_ELEMENTS: u64 = 1_000_000;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeclaredContext {
    pub source: &'static str,
    pub format_version: u32,
    pub architecture: Option<String>,
    pub training_context_tokens: Option<u64>,
}

struct MetadataReader<'a, R> {
    reader: BufReader<&'a mut R>,
    position: u64,
    file_bytes: u64,
}
impl<R: Read + Seek> MetadataReader<'_, R> {
    fn advance(&mut self, bytes: u64) -> Result<(), Error> {
        let end = self
            .position
            .checked_add(bytes)
            .ok_or_else(|| Error::new("gguf_limit", "GGUF metadata offsets overflowed."))?;
        if end > MAX_METADATA_BYTES {
            return Err(Error::new(
                "gguf_limit",
                "The GGUF metadata exceeds 64 MiB.",
            ));
        }
        if end > self.file_bytes {
            return Err(Error::new(
                "invalid_gguf",
                "The GGUF metadata is truncated.",
            ));
        }
        self.position = end;
        Ok(())
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        self.advance(N as u64)?;
        let mut value = [0; N];
        self.reader
            .read_exact(&mut value)
            .map_err(|_| Error::new("gguf_io", "The GGUF metadata could not be read."))?;
        Ok(value)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    fn skip(&mut self, bytes: u64) -> Result<(), Error> {
        self.advance(bytes)?;
        self.reader
            .seek_relative(bytes as i64)
            .map_err(|_| Error::new("gguf_io", "The GGUF metadata could not be inspected."))?;
        Ok(())
    }
    fn string(&mut self, maximum: u64) -> Result<String, Error> {
        let length = self.u64()?;
        if length > maximum {
            return Err(Error::new(
                "gguf_limit",
                "A GGUF metadata string exceeds its size limit.",
            ));
        }
        self.advance(length)?;
        let mut bytes = vec![0; length as usize];
        self.reader
            .read_exact(&mut bytes)
            .map_err(|_| Error::new("gguf_io", "The GGUF metadata could not be read."))?;
        String::from_utf8(bytes).map_err(|_| {
            Error::new(
                "invalid_gguf",
                "GGUF keys and architecture names must be UTF-8.",
            )
        })
    }
    fn skip_string(&mut self) -> Result<(), Error> {
        let length = self.u64()?;
        if length > MAX_STRING_BYTES {
            return Err(Error::new(
                "gguf_limit",
                "A GGUF metadata string exceeds 1 MiB.",
            ));
        }
        self.skip(length)
    }
    fn skip_value(&mut self, kind: u32) -> Result<(), Error> {
        match kind {
            8 => self.skip_string(),
            9 => {
                let element = self.u32()?;
                let count = self.u64()?;
                if count > MAX_ARRAY_ELEMENTS {
                    return Err(Error::new(
                        "gguf_limit",
                        "A GGUF metadata array exceeds one million elements.",
                    ));
                }
                if element == 8 {
                    for _ in 0..count {
                        self.skip_string()?;
                    }
                    Ok(())
                } else {
                    let size = scalar_size(element).ok_or_else(|| {
                        Error::new(
                            "invalid_gguf",
                            "Nested arrays or unknown GGUF metadata types are unsupported.",
                        )
                    })?;
                    self.skip(count.checked_mul(size).ok_or_else(|| {
                        Error::new("gguf_limit", "A GGUF metadata array size overflowed.")
                    })?)
                }
            }
            _ => {
                self.skip(scalar_size(kind).ok_or_else(|| {
                    Error::new("invalid_gguf", "Unknown GGUF metadata value type.")
                })?)
            }
        }
    }
    fn integer(&mut self, kind: u32) -> Result<Option<u64>, Error> {
        let value = match kind {
            0 => self.bytes::<1>()?[0] as i128,
            1 => i8::from_le_bytes(self.bytes()?) as i128,
            2 => u16::from_le_bytes(self.bytes()?) as i128,
            3 => i16::from_le_bytes(self.bytes()?) as i128,
            4 => self.u32()? as i128,
            5 => i32::from_le_bytes(self.bytes()?) as i128,
            10 => self.u64()? as i128,
            11 => i64::from_le_bytes(self.bytes()?) as i128,
            _ => {
                return Err(Error::new(
                    "invalid_gguf",
                    "A declared context length must be an integer.",
                ));
            }
        };
        if value < 0 {
            return Err(Error::new(
                "invalid_gguf",
                "A declared context length cannot be negative.",
            ));
        }
        Ok((value != 0).then_some(value as u64))
    }
}
fn scalar_size(kind: u32) -> Option<u64> {
    match kind {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ModelShape {
    pub block_count: Option<u64>,
    pub embedding_length: Option<u64>,
    pub head_count: Option<u64>,
    pub head_count_kv: Option<u64>,
    pub key_length: Option<u64>,
    pub value_length: Option<u64>,
}

const SHAPE_KEYS: [&str; 6] = [
    "block_count",
    "embedding_length",
    "attention.head_count",
    "attention.head_count_kv",
    "attention.key_length",
    "attention.value_length",
];

pub fn read_gguf<R: Read + Seek>(reader: &mut R) -> Result<DeclaredContext, Error> {
    read_gguf_shape(reader).map(|(declared, _)| declared)
}

/// Visits at most `limit` tensor names after skipping the bounded metadata section.
pub fn visit_gguf_tensor_names<R: Read + Seek>(
    reader: &mut R,
    limit: u64,
    mut visit: impl FnMut(&str),
) -> Result<u64, Error> {
    let file_bytes = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| Error::new("gguf_io", "The GGUF metadata size is unavailable."))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::new("gguf_io", "The GGUF metadata could not be inspected."))?;
    let mut input = MetadataReader {
        reader: BufReader::with_capacity(64 * 1024, reader),
        position: 0,
        file_bytes,
    };
    if input.bytes::<4>()? != *b"GGUF" || !matches!(input.u32()?, 2 | 3) {
        return Err(Error::new(
            "invalid_gguf",
            "The artifact is not a supported GGUF file.",
        ));
    }
    let tensors = input.u64()?;
    let entries = input.u64()?;
    if tensors > MAX_ARRAY_ELEMENTS || entries > MAX_METADATA_ENTRIES {
        return Err(Error::new(
            "gguf_limit",
            "The GGUF header exceeds its tensor or metadata-entry count limit.",
        ));
    }
    for _ in 0..entries {
        input.skip_string()?;
        let kind = input.u32()?;
        input.skip_value(kind)?;
    }
    let visited = tensors.min(limit);
    for _ in 0..visited {
        let name = input.string(1024)?;
        let dimensions = input.u32()?;
        if dimensions == 0 || dimensions > 8 {
            return Err(Error::new(
                "invalid_gguf",
                "A GGUF tensor declares an unsupported dimension count.",
            ));
        }
        input.skip(u64::from(dimensions) * 8 + 12)?;
        visit(&name);
    }
    Ok(visited)
}

pub fn read_gguf_shape<R: Read + Seek>(
    reader: &mut R,
) -> Result<(DeclaredContext, ModelShape), Error> {
    let file_bytes = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| Error::new("gguf_io", "The GGUF metadata size is unavailable."))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::new("gguf_io", "The GGUF metadata could not be inspected."))?;
    let mut input = MetadataReader {
        reader: BufReader::with_capacity(64 * 1024, reader),
        position: 0,
        file_bytes,
    };
    if input.bytes::<4>()? != *b"GGUF" {
        return Err(Error::new(
            "invalid_gguf",
            "The artifact is not a GGUF file.",
        ));
    }
    let format_version = input.u32()?;
    if !matches!(format_version, 2 | 3) {
        return Err(Error::new(
            "unsupported_gguf",
            "Only little-endian GGUF versions 2 and 3 are supported.",
        ));
    }
    let tensors = input.u64()?;
    let entries = input.u64()?;
    if tensors > MAX_ARRAY_ELEMENTS || entries > MAX_METADATA_ENTRIES {
        return Err(Error::new(
            "gguf_limit",
            "The GGUF header exceeds its tensor or metadata-entry count limit.",
        ));
    }
    let mut seen = HashSet::with_capacity(entries as usize);
    let mut architecture = None;
    let mut contexts = Vec::new();
    let mut shape_values = Vec::new();
    for _ in 0..entries {
        let key = input.string(1024)?;
        if key.is_empty() || key.chars().any(char::is_control) || seen.contains(&key) {
            return Err(Error::new(
                "invalid_gguf",
                "GGUF metadata keys must be unique bounded text.",
            ));
        }
        let kind = input.u32()?;
        if key == "general.architecture" {
            if kind != 8 {
                return Err(Error::new(
                    "invalid_gguf",
                    "The GGUF architecture must be text.",
                ));
            }
            let name = input.string(128)?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(Error::new(
                    "invalid_gguf",
                    "The GGUF architecture name is invalid.",
                ));
            }
            architecture = Some(name);
        } else if key.ends_with(".context_length") {
            contexts.push((key.clone(), input.integer(kind)?));
        } else if matches!(kind, 0..=5 | 10 | 11)
            && SHAPE_KEYS.iter().any(|suffix| {
                key.strip_suffix(suffix).is_some_and(|prefix| {
                    prefix.ends_with('.') && !prefix[..prefix.len() - 1].contains('.')
                })
            })
        {
            shape_values.push((key.clone(), input.integer(kind).unwrap_or(None)));
        } else {
            input.skip_value(kind)?;
        }
        seen.insert(key);
    }
    let training_context_tokens = architecture.as_ref().and_then(|architecture| {
        let target = format!("{architecture}.context_length");
        contexts
            .into_iter()
            .find(|(key, _)| key == &target)
            .and_then(|(_, value)| value)
    });
    let shape_value = |suffix: &str| {
        let target = format!("{}.{suffix}", architecture.as_deref()?);
        shape_values
            .iter()
            .find(|(key, _)| *key == target)
            .and_then(|(_, value)| *value)
    };
    let shape = ModelShape {
        block_count: shape_value(SHAPE_KEYS[0]),
        embedding_length: shape_value(SHAPE_KEYS[1]),
        head_count: shape_value(SHAPE_KEYS[2]),
        head_count_kv: shape_value(SHAPE_KEYS[3]),
        key_length: shape_value(SHAPE_KEYS[4]),
        value_length: shape_value(SHAPE_KEYS[5]),
    };
    Ok((
        DeclaredContext {
            source: "gguf:metadata",
            format_version,
            architecture,
            training_context_tokens,
        },
        shape,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }
    fn string(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend((value.len() as u64).to_le_bytes());
        bytes.extend(value);
    }
    fn gguf(version: u32, entries: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(version.to_le_bytes());
        bytes.extend(1_u64.to_le_bytes());
        bytes.extend((entries.len() as u64).to_le_bytes());
        for (key, kind, value) in entries {
            string(&mut bytes, key.as_bytes());
            bytes.extend(kind.to_le_bytes());
            bytes.extend(value);
        }
        bytes
    }
    fn text(value: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        string(&mut bytes, value.as_bytes());
        bytes
    }

    #[test]
    fn launch_options_replace_only_exact_flags_and_preserve_literal_arguments() {
        let original = args(&[
            "--model",
            "{model}",
            "--port",
            "{port}",
            "--slot-save-path",
            "{cache}",
            "-c",
            "4096",
            "--threads=4",
            "--threads-batch",
            "6",
            "--parallel",
            "1",
            "--chat-template",
            "literal $HOME && -c",
            "--ctx-size-extra",
            "unchanged",
        ]);
        let options = LaunchOptions {
            context_tokens: Some(8192),
            threads: Some(8),
            gpu_layers: Some(0),
            parallel_slots: Some(2),
        };
        let resolved = options.resolve(&original).unwrap();
        assert_eq!(resolved.applied, options);
        assert!(
            resolved
                .args
                .windows(2)
                .any(|pair| pair == ["--ctx-size", "8192"])
        );
        assert!(
            resolved
                .args
                .windows(2)
                .any(|pair| pair == ["--threads-batch", "6"])
        );
        assert!(
            resolved
                .args
                .iter()
                .any(|value| value == "literal $HOME && -c")
        );
        assert!(resolved.args.iter().any(|value| value == "{model}"));
        assert!(resolved.args.iter().any(|value| value == "{port}"));
        assert!(resolved.args.iter().any(|value| value == "{cache}"));
        assert_eq!(options.resolve(&resolved.args).unwrap(), resolved);
    }
    #[test]
    fn launch_sentinels_and_conflicts_do_not_become_serving_capacity() {
        let resolved = LaunchOptions::default()
            .resolve(&args(&["-c", "0", "-t", "-1", "-ngl", "all", "-np", "-1"]))
            .unwrap();
        assert_eq!(resolved.applied, LaunchOptions::default());
        assert!(
            LaunchOptions::default()
                .resolve(&args(&["--ctx-size", "2048", "-c", "4096"]))
                .is_err()
        );
        assert!(
            LaunchOptions::default()
                .resolve(&args(&["--parallel", "0"]))
                .is_err()
        );
        assert!(
            LaunchOptions::default()
                .resolve(&args(&["--threads"]))
                .is_err()
        );
        assert!(
            LaunchOptions {
                context_tokens: Some(0),
                ..LaunchOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            LaunchOptions {
                gpu_layers: Some(0),
                ..LaunchOptions::default()
            }
            .validate()
            .is_ok()
        );
        assert!(serde_json::from_str::<LaunchOptions>(r#"{"command":"shell"}"#).is_err());
        assert_eq!(
            LaunchOptions {
                threads: Some(2),
                ..LaunchOptions::default()
            }
            .resolve(&args(&["--", "-c", "literal"]))
            .unwrap()
            .args,
            args(&["--threads", "2", "--", "-c", "literal"])
        );
    }
    #[test]
    fn observed_context_is_never_inferred_by_dividing_a_launch_budget() {
        let props = parse_props(br#"{"n_ctx":16384,"total_slots":4,"default_generation_settings":{"n_ctx":4096},"build_info":"b123"}"#).unwrap();
        assert_eq!(props.per_request_tokens, Some(4096));
        let slots = parse_slots(br#"[{"id":7,"n_ctx":2048,"is_processing":true},{"id":11,"n_ctx":4096,"is_processing":false}]"#).unwrap();
        assert_eq!(slots[0].id, 7);
        assert_eq!(slots[0].context_tokens, Some(2048));
        assert!(props.validate_slots(&slots).is_err());
        let matching =
            parse_props(br#"{"total_slots":2,"build_info":{"unknown_schema":true}}"#).unwrap();
        assert!(matching.validate_capacity(&slots, 2).is_ok());
        assert_eq!(matching.engine_build, None);
        assert_eq!(
            matching.validate_capacity(&slots, 3).unwrap_err().code,
            "slot_capacity_mismatch"
        );
        assert_eq!(
            parse_props(br#"{"n_ctx":16384,"total_slots":4}"#)
                .unwrap()
                .per_request_tokens,
            None
        );
        assert_eq!(
            parse_props(br#"{"total_slots":0,"default_generation_settings":{"n_ctx":"auto"}}"#)
                .unwrap()
                .slots,
            None
        );
        assert!(parse_slots(br#"[{"id":0},{"id":0}]"#).is_err());
        assert!(parse_slots(br#"[{"id":-1}]"#).is_err());
        assert!(parse_slots(b"[]").is_err());
        assert!(parse_props(&vec![b' '; MAX_JSON_BYTES + 1]).is_err());
    }
    #[test]
    fn observations_skip_dense_unknown_fields_and_refuse_excess_slots_and_ambiguous_fields() {
        let padding = "0,".repeat(400_000) + "0";
        let props = format!(
            "{{\"total_slots\":1,\"default_generation_settings\":{{\"n_ctx\":\"a\\u0075to\",\"unknown\":[{padding}]}},\"build_info\":\"b\\u0031\",\"unknown\":[{padding}]}}"
        );
        assert!(props.len() > MAX_JSON_BYTES);
        assert_eq!(
            parse_props(props.as_bytes()).unwrap_err().code,
            "observation_limit"
        );
        let props = format!(
            "{{\"total_slots\":1,\"default_generation_settings\":{{\"n_ctx\":\"a\\u0075to\"}},\"build_info\":\"b\\u0031\",\"unknown\":[{padding}]}}"
        );
        let observed = parse_props(props.as_bytes()).unwrap();
        assert_eq!(observed.per_request_tokens, None);
        assert_eq!(observed.engine_build.as_deref(), Some("b1"));
        let slots =
            format!("[{{\"id\":0,\"n_ctx\":4096,\"is_processing\":null,\"unknown\":[{padding}]}}]");
        assert_eq!(
            parse_slots(slots.as_bytes()).unwrap(),
            vec![SlotObservation {
                id: 0,
                context_tokens: Some(4096),
                is_processing: None
            }]
        );
        let slots = (0..=MAX_SLOTS)
            .map(|id| format!("{{\"id\":{id}}}"))
            .collect::<Vec<_>>();
        assert_eq!(
            parse_slots(format!("[{}]", slots[..MAX_SLOTS].join(",")).as_bytes())
                .unwrap()
                .len(),
            MAX_SLOTS
        );
        assert!(parse_slots(format!("[{}]", slots.join(",")).as_bytes()).is_err());
        assert!(parse_props(br#"{"total_slots":1,"total_slots":2}"#).is_err());
        assert!(parse_slots(br#"[{"id":0,"id":1}]"#).is_err());
        assert!(parse_props(br#"{"default_generation_settings":{"n_ctx":1.5}}"#).is_err());
        assert!(
            parse_props(br#"{"default_generation_settings":{"n_ctx":9223372036854775808}}"#)
                .is_err()
        );
        let huge_build = format!("{{\"build_info\":\"{}\"}}", "b".repeat(900_000));
        assert_eq!(
            parse_props(huge_build.as_bytes()).unwrap().engine_build,
            None
        );
    }
    #[test]
    fn gguf_versions_architecture_order_and_unrelated_arrays_are_supported() {
        let mut strings = 8_u32.to_le_bytes().to_vec();
        strings.extend(2_u64.to_le_bytes());
        string(&mut strings, b"a");
        string(&mut strings, b"b");
        for version in [2, 3] {
            let fixture = gguf(
                version,
                &[
                    ("llama.context_length", 4, 8192_u32.to_le_bytes().to_vec()),
                    ("tokenizer.ggml.tokens", 9, strings.clone()),
                    ("general.architecture", 8, text("llama")),
                    ("other.context_length", 4, 999_u32.to_le_bytes().to_vec()),
                ],
            );
            let mut source = Cursor::new(fixture);
            let declared = read_gguf(&mut source).unwrap();
            assert_eq!(declared.training_context_tokens, Some(8192));
            assert_eq!(declared.architecture.as_deref(), Some("llama"));
            assert_eq!(declared.format_version, version);
        }
    }
    #[test]
    fn unknown_architecture_and_zero_context_remain_unknown() {
        let fixture = gguf(
            3,
            &[
                ("general.architecture", 8, text("unknown")),
                ("llama.context_length", 4, 8192_u32.to_le_bytes().to_vec()),
            ],
        );
        assert_eq!(
            read_gguf(&mut Cursor::new(fixture))
                .unwrap()
                .training_context_tokens,
            None
        );
        let fixture = gguf(
            3,
            &[
                ("general.architecture", 8, text("llama")),
                ("llama.context_length", 4, 0_u32.to_le_bytes().to_vec()),
            ],
        );
        assert_eq!(
            read_gguf(&mut Cursor::new(fixture))
                .unwrap()
                .training_context_tokens,
            None
        );
    }
    #[test]
    fn malformed_truncated_and_unbounded_metadata_never_become_declared_facts() {
        let valid = gguf(
            3,
            &[
                ("general.architecture", 8, text("llama")),
                ("llama.context_length", 4, 8192_u32.to_le_bytes().to_vec()),
            ],
        );
        for length in 0..valid.len() {
            assert!(read_gguf(&mut Cursor::new(&valid[..length])).is_err());
        }
        let nested = gguf(
            3,
            &[(
                "array",
                9,
                [
                    9_u32.to_le_bytes().as_slice(),
                    1_u64.to_le_bytes().as_slice(),
                ]
                .concat(),
            )],
        );
        assert!(read_gguf(&mut Cursor::new(nested)).is_err());
        let huge = gguf(
            3,
            &[(
                "array",
                9,
                [
                    0_u32.to_le_bytes().as_slice(),
                    (MAX_ARRAY_ELEMENTS + 1).to_le_bytes().as_slice(),
                ]
                .concat(),
            )],
        );
        assert_eq!(
            read_gguf(&mut Cursor::new(huge)).unwrap_err().code,
            "gguf_limit"
        );
        let duplicate = gguf(
            3,
            &[
                ("general.architecture", 8, text("llama")),
                ("general.architecture", 8, text("other")),
            ],
        );
        assert!(read_gguf(&mut Cursor::new(duplicate)).is_err());
        let wrong = gguf(
            3,
            &[
                ("general.architecture", 8, text("llama")),
                ("llama.context_length", 6, 2_f32.to_le_bytes().to_vec()),
            ],
        );
        assert!(read_gguf(&mut Cursor::new(wrong)).is_err());
        assert!(read_gguf(&mut Cursor::new(gguf(1, &[]))).is_err());
        let mut count = gguf(3, &[]);
        count[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            read_gguf(&mut Cursor::new(count)).unwrap_err().code,
            "gguf_limit"
        );
        let huge_string = gguf(3, &[("ignored", 8, u64::MAX.to_le_bytes().to_vec())]);
        assert_eq!(
            read_gguf(&mut Cursor::new(huge_string)).unwrap_err().code,
            "gguf_limit"
        );
        let invalid_utf8 = gguf(
            3,
            &[("general.architecture", 8, {
                let mut bytes = 1_u64.to_le_bytes().to_vec();
                bytes.push(0xff);
                bytes
            })],
        );
        assert!(read_gguf(&mut Cursor::new(invalid_utf8)).is_err());
        let negative = gguf(
            3,
            &[
                ("general.architecture", 8, text("llama")),
                ("llama.context_length", 5, (-1_i32).to_le_bytes().to_vec()),
            ],
        );
        assert!(read_gguf(&mut Cursor::new(negative)).is_err());
    }
    #[test]
    fn metadata_byte_limit_rejects_before_reading_or_allocating_the_payload() {
        struct NoRead;
        impl Read for NoRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("A bounded inspection must reject before reading this payload")
            }
        }
        impl Seek for NoRead {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                panic!("A bounded inspection must reject before seeking into this payload")
            }
        }
        let mut source = NoRead;
        let mut input = MetadataReader {
            reader: BufReader::new(&mut source),
            position: MAX_METADATA_BYTES - 4,
            file_bytes: u64::MAX,
        };
        assert_eq!(input.bytes::<8>().unwrap_err().code, "gguf_limit");
    }

    #[test]
    #[ignore = "requires JUICED_TEST_GGUF; observes real metadata without loading model weights"]
    fn real_gguf_metadata_observation() {
        let path =
            std::env::var_os("JUICED_TEST_GGUF").expect("Set JUICED_TEST_GGUF to an existing GGUF");
        let mut file = std::fs::File::open(path).unwrap();
        let declared = read_gguf(&mut file).unwrap();
        assert!(declared.architecture.is_some());
        assert!(
            declared
                .training_context_tokens
                .is_none_or(|value| value > 0)
        );
        println!("{}", serde_json::to_string(&declared).unwrap());
    }
}
