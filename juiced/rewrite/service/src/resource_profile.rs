use crate::{admission::Priority, config::Operation};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

const MAX_BYTES: u64 = i64::MAX as u64;
const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_BATCH: u32 = 4096;
const MAX_OUTPUT_TOKENS: u32 = i32::MAX as u32;
const MAX_DEPTH: usize = 64;
const TOKEN_ALIASES: [&str; 5] = [
    "max_tokens",
    "max_completion_tokens",
    "n_predict",
    "max_output_tokens",
    "num_predict",
];
const PROBABILITY_ALIASES: [&str; 4] = ["logprobs", "top_logprobs", "prompt_logprobs", "n_probs"];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub input_bytes: u64,
    pub batch: u32,
    pub output_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(try_from = "ProfileFields")]
pub struct Profile {
    pub schema_version: u32,
    pub launch_fingerprint: String,
    pub priority: Priority,
    pub completion_ms: u64,
    pub load_bytes: u64,
    pub resident_bytes: u64,
    pub request_bytes: u64,
    pub state_bytes: Option<u64>,
    pub request_limits: Limits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFields {
    schema_version: u32,
    launch_fingerprint: String,
    priority: Priority,
    completion_ms: u64,
    load_bytes: u64,
    resident_bytes: u64,
    request_bytes: u64,
    state_bytes: Option<u64>,
    request_limits: Limits,
}
impl TryFrom<ProfileFields> for Profile {
    type Error = Error;
    fn try_from(fields: ProfileFields) -> Result<Self, Self::Error> {
        let profile = Self {
            schema_version: fields.schema_version,
            launch_fingerprint: fields.launch_fingerprint,
            priority: fields.priority,
            completion_ms: fields.completion_ms,
            load_bytes: fields.load_bytes,
            resident_bytes: fields.resident_bytes,
            request_bytes: fields.request_bytes,
            state_bytes: fields.state_bytes,
            request_limits: fields.request_limits,
        };
        profile.validate()?;
        Ok(profile)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
    pub message: &'static str,
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl Error {
    fn profile() -> Self {
        Self {
            status: 400,
            code: "admission_profile_invalid",
            message: "A versioned host resource profile requires a launch identity and explicit bounded ceilings.",
        }
    }
    fn invalid() -> Self {
        Self {
            status: 400,
            code: "invalid_request",
            message: "The protected text request has malformed or conflicting fields.",
        }
    }
    fn unsupported() -> Self {
        Self {
            status: 503,
            code: "workload_unqualified",
            message: "This operation or request shape has no selected protected text resource contract.",
        }
    }
    fn limit() -> Self {
        Self {
            status: 413,
            code: "workload_limit_exceeded",
            message: "The request exceeds its declared text resource envelope.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Workload {
    pub input_bytes: u64,
    pub batch: u32,
    pub output_tokens: u32,
}

pub struct Envelope {
    pub body: Value,
    pub workload: Workload,
}
impl fmt::Debug for Envelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Envelope")
            .field("workload", &self.workload)
            .finish_non_exhaustive()
    }
}

impl Profile {
    pub fn validate(&self) -> Result<(), Error> {
        let bounded_bytes = |bytes| (1..=MAX_BYTES).contains(&bytes);
        if self.schema_version != 1
            || !fingerprint(&self.launch_fingerprint)
            || !(1..=300000).contains(&self.completion_ms)
            || !bounded_bytes(self.load_bytes)
            || !bounded_bytes(self.resident_bytes)
            || !bounded_bytes(self.request_bytes)
            || self.state_bytes.is_some_and(|bytes| !bounded_bytes(bytes))
            || self.load_bytes < self.resident_bytes
            || !(1..=MAX_INPUT_BYTES).contains(&self.request_limits.input_bytes)
            || !(1..=MAX_BATCH).contains(&self.request_limits.batch)
            || !(1..=MAX_OUTPUT_TOKENS).contains(&self.request_limits.output_tokens)
        {
            return Err(Error::profile());
        }
        self.load_bytes
            .checked_add(self.request_bytes)
            .and_then(|bytes| bytes.checked_add(self.state_bytes.unwrap_or(0)))
            .ok_or_else(Error::profile)?;
        Ok(())
    }

    pub fn verify_fingerprint(&self, actual: &str) -> Result<(), Error> {
        self.validate()?;
        if actual != self.launch_fingerprint {
            return Err(Error {
                status: 409,
                code: "admission_profile_mismatch",
                message: "The verified launch differs from the declared resource profile.",
            });
        }
        Ok(())
    }

    pub fn resolve(&self, operation: Operation, mut body: Value) -> Result<Envelope, Error> {
        self.validate()?;
        if !matches!(
            operation,
            Operation::Chat | Operation::Completion | Operation::Responses
        ) {
            return Err(Error::unsupported());
        }
        let original_bytes = storage_ceiling(&body, self.request_limits.input_bytes)?;
        let request = body.as_object_mut().ok_or_else(Error::invalid)?;
        request.remove("priority");
        for key in request.keys() {
            let input = match operation {
                Operation::Chat => key == "messages",
                Operation::Completion => key == "prompt",
                Operation::Responses => matches!(key.as_str(), "input" | "instructions"),
                _ => false,
            };
            if !input && !common_field(key) {
                return Err(Error::unsupported());
            }
        }
        let model = request
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(Error::invalid)?;
        if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            return Err(Error::invalid());
        }
        let mut output_tokens = None;
        for key in TOKEN_ALIASES {
            if let Some(value) = request.get(key) {
                if key == "num_predict"
                    || (matches!(operation, Operation::Responses) != (key == "max_output_tokens"))
                {
                    return Err(Error::unsupported());
                }
                merge_bound(&mut output_tokens, positive(value)?)?;
            }
        }
        let output_tokens = output_tokens.ok_or_else(Error::invalid)?;
        let mut batch = None;
        for key in ["n", "n_cmpl"] {
            if let Some(value) = request.get(key) {
                merge_bound(&mut batch, positive(value)?)?;
            }
        }
        let batch = batch.unwrap_or(1);
        if batch > self.request_limits.batch || output_tokens > self.request_limits.output_tokens {
            return Err(Error::limit());
        }
        validate_options(request)?;
        match operation {
            Operation::Chat => messages(request.get("messages"), false)?,
            Operation::Completion => {
                if !request.get("prompt").is_some_and(Value::is_string) {
                    return Err(Error::unsupported());
                }
            }
            Operation::Responses => {
                if request
                    .get("instructions")
                    .is_some_and(|value| !value.is_string())
                {
                    return Err(Error::invalid());
                }
                match request.get("input") {
                    Some(Value::String(_)) => {}
                    Some(Value::Array(_)) => messages(request.get("input"), true)?,
                    _ => return Err(Error::unsupported()),
                }
            }
            _ => return Err(Error::unsupported()),
        }
        for key in TOKEN_ALIASES {
            request.remove(key);
        }
        request.insert(
            if matches!(operation, Operation::Responses) {
                "max_output_tokens"
            } else {
                "max_tokens"
            }
            .into(),
            Value::from(output_tokens),
        );
        request.remove("n");
        request.insert("n_cmpl".into(), Value::from(batch));
        for key in PROBABILITY_ALIASES {
            request.remove(key);
        }
        request.insert("n_probs".into(), Value::from(0));
        let input_bytes =
            original_bytes.max(storage_ceiling(&body, self.request_limits.input_bytes)?);
        Ok(Envelope {
            body,
            workload: Workload {
                input_bytes,
                batch,
                output_tokens,
            },
        })
    }
}

fn fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn common_field(key: &str) -> bool {
    matches!(
        key,
        "model"
            | "stream"
            | "stream_options"
            | "temperature"
            | "top_p"
            | "top_k"
            | "min_p"
            | "seed"
            | "stop"
            | "repeat_penalty"
            | "frequency_penalty"
            | "presence_penalty"
            | "typical_p"
            | "repeat_last_n"
            | "cache_prompt"
            | "ignore_eos"
            | "max_tokens"
            | "max_completion_tokens"
            | "n_predict"
            | "max_output_tokens"
            | "num_predict"
            | "n"
            | "n_cmpl"
            | "logprobs"
            | "top_logprobs"
            | "prompt_logprobs"
            | "n_probs"
    )
}
fn positive(value: &Value) -> Result<u32, Error> {
    value
        .as_u64()
        .filter(|value| *value > 0 && *value <= MAX_OUTPUT_TOKENS as u64)
        .map(|value| value as u32)
        .ok_or_else(Error::invalid)
}
fn merge_bound(previous: &mut Option<u32>, value: u32) -> Result<(), Error> {
    if previous.is_some_and(|previous| previous != value) {
        return Err(Error::invalid());
    }
    *previous = Some(value);
    Ok(())
}
fn object_fields(value: &Value, fields: &[&str]) -> Result<(), Error> {
    let object = value.as_object().ok_or_else(Error::invalid)?;
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(Error::unsupported());
    }
    Ok(())
}
fn messages(value: Option<&Value>, responses: bool) -> Result<(), Error> {
    let values = value
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(Error::invalid)?;
    for message in values {
        object_fields(
            message,
            if responses {
                &["type", "role", "content"]
            } else {
                &["role", "content", "name"]
            },
        )?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(Error::invalid)?;
        if !matches!(role, "user" | "system" | "developer" | "assistant") {
            return Err(Error::unsupported());
        }
        if responses {
            if message.get("type").is_some_and(|value| value != "message")
                || (role == "assistant"
                    && message.get("type").and_then(Value::as_str) != Some("message"))
            {
                return Err(Error::unsupported());
            }
        } else if let Some(name) = message.get("name") {
            let name = name.as_str().ok_or_else(Error::invalid)?;
            if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                return Err(Error::invalid());
            }
        }
        let content = message.get("content").ok_or_else(Error::invalid)?;
        if content.is_string() {
            continue;
        }
        let parts = content.as_array().ok_or_else(Error::unsupported)?;
        for part in parts {
            object_fields(part, &["type", "text"])?;
            let kind = part
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(Error::invalid)?;
            let supported = if responses {
                kind == "input_text" || (role == "assistant" && kind == "output_text")
            } else {
                kind == "text"
            };
            if !supported {
                return Err(Error::unsupported());
            }
            if !part.get("text").is_some_and(Value::is_string) {
                return Err(Error::invalid());
            }
        }
    }
    Ok(())
}

fn validate_options(request: &Map<String, Value>) -> Result<(), Error> {
    for key in ["stream", "cache_prompt", "ignore_eos"] {
        if request.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(Error::invalid());
        }
    }
    if let Some(options) = request.get("stream_options") {
        object_fields(options, &["include_usage"])?;
        if options
            .get("include_usage")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(Error::invalid());
        }
    }
    for key in PROBABILITY_ALIASES {
        if request
            .get(key)
            .is_some_and(|value| !(value.is_null() || value == false || value.as_u64() == Some(0)))
        {
            return Err(Error::unsupported());
        }
    }
    for (key, minimum, maximum) in [
        ("temperature", 0.0, f32::MAX as f64),
        ("top_p", 0.0, 1.0),
        ("min_p", 0.0, 1.0),
        ("typical_p", 0.0, 1.0),
        ("repeat_penalty", 0.0, f32::MAX as f64),
        ("frequency_penalty", -2.0, 2.0),
        ("presence_penalty", -2.0, 2.0),
    ] {
        if let Some(value) = request.get(key) {
            let value = value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(Error::invalid)?;
            if value < minimum || value > maximum {
                return Err(Error::invalid());
            }
        }
    }
    for key in ["top_k", "repeat_last_n"] {
        if let Some(value) = request.get(key)
            && !value.as_u64().is_some_and(|value| value <= i32::MAX as u64)
        {
            return Err(Error::invalid());
        }
    }
    if request.get("seed").is_some_and(|value| {
        value.as_i64() != Some(-1) && !value.as_u64().is_some_and(|value| value <= u32::MAX as u64)
    }) {
        return Err(Error::invalid());
    }
    if let Some(stop) = request.get("stop")
        && !stop.is_string()
        && !stop
            .as_array()
            .is_some_and(|values| values.len() <= 16 && values.iter().all(Value::is_string))
    {
        return Err(Error::invalid());
    }
    Ok(())
}
fn add(left: u64, right: u64) -> Result<u64, Error> {
    left.checked_add(right).ok_or_else(Error::limit)
}
fn text_bytes(length: usize) -> Result<u64, Error> {
    u64::try_from(length)
        .ok()
        .and_then(|length| length.checked_mul(6))
        .ok_or_else(Error::limit)
}
fn storage_ceiling(value: &Value, maximum: u64) -> Result<u64, Error> {
    fn visit(value: &Value, depth: usize, maximum: u64) -> Result<u64, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::unsupported());
        }
        let mut bytes = 64;
        match value {
            Value::String(value) => bytes = add(bytes, text_bytes(value.len())?)?,
            Value::Array(values) => {
                for value in values {
                    bytes = add(bytes, visit(value, depth + 1, maximum)?)?;
                    if bytes > maximum {
                        return Err(Error::limit());
                    }
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    bytes = add(bytes, text_bytes(key.len())?)?;
                    bytes = add(bytes, visit(value, depth + 1, maximum)?)?;
                    if bytes > maximum {
                        return Err(Error::limit());
                    }
                }
            }
            _ => {}
        }
        if bytes > maximum {
            return Err(Error::limit());
        }
        Ok(bytes)
    }
    visit(value, 0, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile() -> Profile {
        Profile {
            schema_version: 1,
            launch_fingerprint: "a".repeat(64),
            priority: Priority::Interactive,
            completion_ms: 3000,
            load_bytes: 4096,
            resident_bytes: 2048,
            request_bytes: 1024,
            state_bytes: Some(512),
            request_limits: Limits {
                input_bytes: MAX_INPUT_BYTES,
                batch: 4,
                output_tokens: 32,
            },
        }
    }
    fn completion() -> Value {
        json!({"model":"protected-model","prompt":"bounded text","max_tokens":8})
    }
    fn rejected(operation: Operation, body: Value, status: u16, code: &str) {
        let error = profile().resolve(operation, body).unwrap_err();
        assert_eq!(error.status, status);
        assert_eq!(error.code, code);
    }

    #[test]
    fn strict_profile_schema_round_trips_and_rejects_unknown_or_missing_authority() {
        let value = serde_json::to_value(profile()).unwrap();
        assert_eq!(
            serde_json::from_value::<Profile>(value.clone()).unwrap(),
            profile()
        );
        for key in [
            "schema_version",
            "launch_fingerprint",
            "priority",
            "completion_ms",
            "load_bytes",
            "resident_bytes",
            "request_bytes",
            "request_limits",
        ] {
            let mut candidate = value.clone();
            candidate.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<Profile>(candidate).is_err(),
                "{key}"
            );
        }
        for candidate in [
            json!({"pool":"gpu"}),
            json!({"priority":"operator"}),
            json!({"request_limits":{"input_bytes":1024,"batch":1,"output_tokens":1,"images":1}}),
        ] {
            let mut candidate_profile = value.clone();
            candidate_profile
                .as_object_mut()
                .unwrap()
                .extend(candidate.as_object().unwrap().clone());
            assert!(serde_json::from_value::<Profile>(candidate_profile).is_err());
        }
        let duplicate =
            serde_json::to_string(&profile())
                .unwrap()
                .replacen("{", "{\"completion_ms\":4,", 1);
        assert!(serde_json::from_str::<Profile>(&duplicate).is_err());
    }

    #[test]
    fn profile_validation_checks_phase_ceilings_limits_and_overflow() {
        let base = serde_json::to_value(profile()).unwrap();
        for (key, value) in [
            ("schema_version", json!(2)),
            ("launch_fingerprint", json!("A".repeat(64))),
            ("launch_fingerprint", json!("a".repeat(63))),
            ("launch_fingerprint", json!("g".repeat(64))),
            ("completion_ms", json!(0)),
            ("completion_ms", json!(300001)),
            ("load_bytes", json!(0)),
            ("load_bytes", json!(1024)),
            ("load_bytes", json!(u64::MAX)),
            ("resident_bytes", json!(0)),
            ("request_bytes", json!(0)),
            ("state_bytes", json!(0)),
        ] {
            let mut candidate = base.clone();
            candidate[key] = value;
            assert!(
                serde_json::from_value::<Profile>(candidate).is_err(),
                "{key}"
            );
        }
        for (key, value) in [
            ("input_bytes", json!(0)),
            ("input_bytes", json!(MAX_INPUT_BYTES + 1)),
            ("batch", json!(0)),
            ("batch", json!(MAX_BATCH + 1)),
            ("output_tokens", json!(0)),
            ("output_tokens", json!(MAX_OUTPUT_TOKENS + 1)),
        ] {
            let mut candidate = base.clone();
            candidate["request_limits"][key] = value;
            assert!(
                serde_json::from_value::<Profile>(candidate).is_err(),
                "{key}"
            );
        }
        let mut overflow = profile();
        overflow.load_bytes = MAX_BYTES;
        overflow.request_bytes = MAX_BYTES;
        overflow.state_bytes = Some(MAX_BYTES);
        assert_eq!(
            overflow.validate().unwrap_err().code,
            "admission_profile_invalid"
        );
        let mut boundary = profile();
        boundary.completion_ms = 300000;
        boundary.load_bytes = MAX_BYTES;
        boundary.state_bytes = None;
        boundary.request_limits = Limits {
            input_bytes: MAX_INPUT_BYTES,
            batch: MAX_BATCH,
            output_tokens: MAX_OUTPUT_TOKENS,
        };
        assert!(boundary.validate().is_ok());
    }

    #[test]
    fn launch_identity_requires_exact_current_fingerprint() {
        assert!(profile().verify_fingerprint(&"a".repeat(64)).is_ok());
        for fingerprint in [
            "b".repeat(64),
            "A".repeat(64),
            "a".repeat(63),
            String::new(),
        ] {
            let error = profile().verify_fingerprint(&fingerprint).unwrap_err();
            assert_eq!(
                (error.status, error.code),
                (409, "admission_profile_mismatch")
            );
        }
    }

    #[test]
    fn completion_normalizes_equal_token_and_count_aliases_without_copying_text() {
        let body = json!({"model":"protected-model","prompt":"persistent prompt allocation λ","max_tokens":8,"max_completion_tokens":8,"n_predict":8,"n":2,"n_cmpl":2,"priority":"essential","logprobs":false,"top_logprobs":null,"n_probs":0,"temperature":0.5});
        let pointer = body["prompt"].as_str().unwrap().as_ptr();
        let original = storage_ceiling(&body, MAX_INPUT_BYTES).unwrap();
        let envelope = profile().resolve(Operation::Completion, body).unwrap();
        assert_eq!(
            envelope.body,
            json!({"model":"protected-model","prompt":"persistent prompt allocation λ","max_tokens":8,"n_cmpl":2,"n_probs":0,"temperature":0.5})
        );
        assert_eq!(envelope.body["prompt"].as_str().unwrap().as_ptr(), pointer);
        assert_eq!(
            envelope.workload,
            Workload {
                input_bytes: original
                    .max(storage_ceiling(&envelope.body, MAX_INPUT_BYTES).unwrap()),
                batch: 2,
                output_tokens: 8
            }
        );
        assert!(!format!("{envelope:?}").contains("persistent prompt"));
    }

    #[test]
    fn chat_text_messages_and_sampling_options_remain_exact() {
        let body = json!({"model":"protected-model","messages":[{"role":"system","content":"bounded"},{"role":"user","name":"caller","content":[{"type":"text","text":"Unicode λ🌍"}]}],"max_completion_tokens":4,"stream":true,"stream_options":{"include_usage":true},"cache_prompt":true,"ignore_eos":true,"top_p":0.8,"min_p":0.1,"typical_p":1,"top_k":0,"repeat_last_n":0,"seed":-1,"stop":["end"],"frequency_penalty":-2,"presence_penalty":2,"repeat_penalty":1});
        let mut expected = body.clone();
        expected
            .as_object_mut()
            .unwrap()
            .remove("max_completion_tokens");
        expected["max_tokens"] = json!(4);
        expected["n_cmpl"] = json!(1);
        expected["n_probs"] = json!(0);
        assert_eq!(
            profile().resolve(Operation::Chat, body).unwrap().body,
            expected
        );
    }

    #[test]
    fn responses_preserves_text_structure_and_uses_only_selected_token_field() {
        for input in [
            json!("text"),
            json!([{"role":"user","content":[{"type":"input_text","text":"text"}]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"prior"}]}]),
        ] {
            let body = json!({"model":"protected-model","input":input,"instructions":"bounded","max_output_tokens":4,"n":1});
            let envelope = profile().resolve(Operation::Responses, body).unwrap();
            assert_eq!(
                envelope.body,
                json!({"model":"protected-model","input":input,"instructions":"bounded","max_output_tokens":4,"n_cmpl":1,"n_probs":0})
            );
            assert_eq!(envelope.workload.batch, 1);
            assert_eq!(envelope.workload.output_tokens, 4);
        }
    }

    #[test]
    fn token_aliases_are_operation_specific_and_must_agree() {
        for alias in ["max_tokens", "max_completion_tokens", "n_predict"] {
            let mut body = completion();
            body.as_object_mut().unwrap().remove("max_tokens");
            body[alias] = json!(8);
            assert_eq!(
                profile().resolve(Operation::Completion, body).unwrap().body["max_tokens"],
                8
            );
            rejected(
                Operation::Responses,
                json!({"model":"m","input":"text","max_output_tokens":8,alias:8}),
                503,
                "workload_unqualified",
            );
        }
        for alias in ["max_output_tokens", "num_predict"] {
            let mut body = completion();
            body[alias] = json!(8);
            rejected(Operation::Completion, body, 503, "workload_unqualified");
        }
        for value in [
            json!(-1),
            json!(0),
            json!(1.0),
            json!("8"),
            Value::Null,
            json!(u64::MAX),
            json!(i32::MAX as u64 + 1),
        ] {
            let mut body = completion();
            body["max_tokens"] = value;
            rejected(Operation::Completion, body, 400, "invalid_request");
        }
        let mut missing = completion();
        missing.as_object_mut().unwrap().remove("max_tokens");
        rejected(Operation::Completion, missing, 400, "invalid_request");
        for (key, value) in [
            ("n_predict", json!(7)),
            ("n", json!(2)),
            ("n_cmpl", json!(0)),
        ] {
            let mut body = completion();
            body["n_cmpl"] = json!(1);
            body[key] = value;
            rejected(Operation::Completion, body, 400, "invalid_request");
        }
    }

    #[test]
    fn unsupported_operations_parameters_and_caller_slots_have_no_envelope() {
        for operation in [
            Operation::Embeddings,
            Operation::TokenCount,
            Operation::Transcription,
        ] {
            rejected(operation, completion(), 503, "workload_unqualified");
        }
        for (key, value) in [
            ("extra_params", json!({"max_tokens":1000})),
            ("id_slot", json!(0)),
            ("tools", json!([])),
            ("tool_choice", json!("none")),
            ("best_of", json!(1)),
            ("grammar", json!("root ::= [a-z]")),
            ("response_format", json!({"type":"json_object"})),
            ("chat_template_kwargs", json!({})),
            ("return_tokens", json!(true)),
            ("n_keep", json!(100)),
            ("unrecognized_private_marker", json!("never-log")),
        ] {
            let mut body = completion();
            body[key] = value;
            rejected(Operation::Completion, body, 503, "workload_unqualified");
        }
    }

    #[test]
    fn tools_multimodal_and_hidden_nested_parameters_are_rejected() {
        for message in [
            json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"private"}}]}),
            json!({"role":"tool","content":"tool output"}),
            json!({"role":"assistant","content":"text","tool_calls":[]}),
            json!({"role":"user","content":[{"type":"text","text":"safe","image_url":"hidden"}]}),
        ] {
            rejected(
                Operation::Chat,
                json!({"model":"m","messages":[message],"max_tokens":4}),
                503,
                "workload_unqualified",
            );
        }
        for input in [
            json!([{"type":"function_call","call_id":"a","arguments":"private"}]),
            json!([{"role":"user","content":[{"type":"input_image","image_url":"private"}]}]),
            json!([{"role":"assistant","content":"missing selected type"}]),
        ] {
            rejected(
                Operation::Responses,
                json!({"model":"m","input":input,"max_output_tokens":4}),
                503,
                "workload_unqualified",
            );
        }
        rejected(
            Operation::Completion,
            json!({"model":"m","prompt":["first","second"],"max_tokens":4}),
            503,
            "workload_unqualified",
        );
    }

    #[test]
    fn malformed_text_and_sampling_fields_have_static_errors() {
        for (key, value) in [
            ("model", json!("")),
            ("model", json!("a\n")),
            ("temperature", json!(-1)),
            ("top_p", json!(2)),
            ("min_p", json!("0.1")),
            ("top_k", json!(-1)),
            ("seed", json!(u64::MAX)),
            ("repeat_last_n", json!(1.5)),
            ("stream", json!(1)),
            ("stop", json!(["text", 9])),
            ("cache_prompt", Value::Null),
            ("stream_options", json!({"include_usage":"yes"})),
        ] {
            let mut body = completion();
            body[key] = value;
            rejected(Operation::Completion, body, 400, "invalid_request");
        }
        for messages in [
            json!([]),
            json!([{"role":"user"}]),
            json!([{"content":"text"}]),
            json!([{"role":"user","content":[{"type":"text","text":null}]}]),
        ] {
            rejected(
                Operation::Chat,
                json!({"model":"m","messages":messages,"max_tokens":4}),
                400,
                "invalid_request",
            );
        }
        let mut body = completion();
        body["private_marker_that_must_not_be_echoed"] = json!("never-echo-source");
        let error = profile().resolve(Operation::Completion, body).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private_marker"));
        assert!(!format!("{error:?} {error}").contains("never-echo"));
    }

    #[test]
    fn probability_outputs_are_disabled_in_actual_forwarded_fields() {
        for alias in PROBABILITY_ALIASES {
            for value in [json!(true), json!(1), json!("0"), json!({}), json!([])] {
                let mut body = completion();
                body[alias] = value;
                rejected(Operation::Completion, body, 503, "workload_unqualified");
            }
            for value in [Value::Null, json!(false), json!(0)] {
                let mut body = completion();
                body[alias] = value;
                let resolved = profile().resolve(Operation::Completion, body).unwrap();
                assert_eq!(resolved.body["n_probs"], 0);
                for removed in ["logprobs", "top_logprobs", "prompt_logprobs"] {
                    assert!(resolved.body.get(removed).is_none())
                }
            }
        }
    }

    #[test]
    fn exact_input_output_and_batch_limits_are_enforced_after_normalization() {
        let body = completion();
        let envelope = profile()
            .resolve(Operation::Completion, body.clone())
            .unwrap();
        let mut bounded = profile();
        bounded.request_limits.input_bytes = envelope.workload.input_bytes;
        assert!(bounded.resolve(Operation::Completion, body.clone()).is_ok());
        bounded.request_limits.input_bytes -= 1;
        assert_eq!(
            bounded
                .resolve(Operation::Completion, body)
                .unwrap_err()
                .code,
            "workload_limit_exceeded"
        );
        for (key, value) in [("max_tokens", json!(33)), ("n", json!(5))] {
            let mut body = completion();
            body[key] = value;
            rejected(Operation::Completion, body, 413, "workload_limit_exceeded");
        }
        let mut body = completion();
        body["max_tokens"] = json!(32);
        body["n"] = json!(4);
        let envelope = profile().resolve(Operation::Completion, body).unwrap();
        assert_eq!(
            (envelope.workload.output_tokens, envelope.workload.batch),
            (32, 4)
        );
    }

    #[test]
    fn storage_measure_counts_utf8_keys_nodes_and_checked_arithmetic_without_serialization() {
        assert_eq!(storage_ceiling(&Value::Null, MAX_INPUT_BYTES).unwrap(), 64);
        assert_eq!(
            storage_ceiling(&json!("λ"), MAX_INPUT_BYTES).unwrap(),
            64 + 2 * 6
        );
        assert_eq!(
            storage_ceiling(&json!({"λ":[true,"a"]}), MAX_INPUT_BYTES).unwrap(),
            64 + 12 + 64 + 64 + 64 + 6
        );
        assert_eq!(
            add(u64::MAX, 1).unwrap_err().code,
            "workload_limit_exceeded"
        );
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            text_bytes(usize::MAX).unwrap_err().code,
            "workload_limit_exceeded"
        );
        let mut nested = Value::Null;
        for _ in 0..64 {
            nested = Value::Array(vec![nested]);
        }
        assert!(storage_ceiling(&nested, MAX_INPUT_BYTES).is_ok());
        nested = Value::Array(vec![nested]);
        assert_eq!(
            storage_ceiling(&nested, MAX_INPUT_BYTES).unwrap_err().code,
            "workload_unqualified"
        );
        let wide = Value::Array(vec![Value::Null; (MAX_INPUT_BYTES / 64) as usize]);
        assert_eq!(
            storage_ceiling(&wide, MAX_INPUT_BYTES).unwrap_err().code,
            "workload_limit_exceeded"
        );
    }
}
