use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[cfg(target_os = "linux")]
struct IdentityMapping {
    address: usize,
    bytes: usize,
    _file: std::fs::File,
}
#[cfg(target_os = "linux")]
impl IdentityMapping {
    fn open(path: &std::path::Path) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let bytes = usize::try_from(metadata.len())
            .map_err(|_| std::io::Error::other("Fixture mapping is too large"))?;
        if !metadata.is_file() || bytes == 0 || bytes > 16 * 1024 * 1024 {
            return Err(std::io::Error::other(
                "Fixture mapping must be a bounded regular file",
            ));
        }
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            address: address as usize,
            bytes,
            _file: file,
        })
    }
}
#[cfg(target_os = "linux")]
impl Drop for IdentityMapping {
    fn drop(&mut self) {
        // The fixture retains this read-only mapping without calling or dereferencing its contents.
        unsafe {
            libc::munmap(self.address as *mut libc::c_void, self.bytes);
        }
    }
}

struct Fixture {
    #[cfg(target_os = "linux")]
    _identity_map: Option<IdentityMapping>,
    events: Option<PathBuf>,
    finite_stream: bool,
    report_timings: bool,
    chatty_bytes: usize,
    inference_delay: u64,
    slot_delay: u64,
    speech_mode: String,
    speech_status: u16,
    speech_release: Option<PathBuf>,
    speech_physical_release: Option<PathBuf>,
    parallel: u32,
    context: u32,
    cache: Option<PathBuf>,
    slot: Mutex<Vec<u8>>,
    prompt_cache: Option<Mutex<Vec<Vec<String>>>>,
    restores: AtomicU64,
    state_controls: StateControls,
    inference_started: AtomicU64,
    saves_started: AtomicU64,
    restores_started: AtomicU64,
    inspections_started: AtomicU64,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum StateOperation {
    Save,
    Restore,
    InspectSlots,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StateReply {
    Valid,
    Malformed,
    Interrupted,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
enum StateControl {
    Arm {
        operation: StateOperation,
        hold_effect: bool,
        hold_eof: bool,
        reply: StateReply,
        status: u16,
    },
    ReleaseEffect {
        operation_id: u64,
    },
    ReleaseEof {
        operation_id: u64,
    },
}

struct StateGate {
    open: Mutex<bool>,
    changed: Condvar,
    changed_async: tokio::sync::Notify,
}
impl StateGate {
    fn new(open: bool) -> Self {
        Self {
            open: Mutex::new(open),
            changed: Condvar::new(),
            changed_async: tokio::sync::Notify::new(),
        }
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
        self.changed_async.notify_waiters();
    }
    fn wait_blocking(&self) -> bool {
        let (open, _) = self
            .changed
            .wait_timeout_while(self.open.lock().unwrap(), Duration::from_secs(15), |open| {
                !*open
            })
            .unwrap();
        *open
    }
    async fn wait(&self) -> bool {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let changed = self.changed_async.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if *self.open.lock().unwrap() {
                    return;
                }
                changed.await;
            }
        })
        .await
        .is_ok()
    }
}

struct StateJob {
    id: u64,
    operation: StateOperation,
    effect: StateGate,
    eof: StateGate,
    reply: StateReply,
    status: u16,
    physical_started: AtomicBool,
    effect_started: AtomicBool,
    effect_completed: AtomicBool,
    physical_settled: AtomicBool,
    bytes: AtomicU64,
    reply_started: AtomicBool,
    reply_eof: AtomicBool,
    transport_dropped: AtomicBool,
    watchdog_expired: AtomicBool,
}
impl StateJob {
    fn terminal(&self) -> bool {
        self.physical_settled.load(Ordering::Acquire)
            && (self.reply_eof.load(Ordering::Acquire)
                || self.transport_dropped.load(Ordering::Acquire))
    }
    fn view(&self) -> Value {
        json!({"id":self.id,"operation":self.operation,
            "physical_started":self.physical_started.load(Ordering::Acquire),
            "effect_started":self.effect_started.load(Ordering::Acquire),
            "effect_completed":self.effect_completed.load(Ordering::Acquire),
            "physical_settled":self.physical_settled.load(Ordering::Acquire),
            "bytes":self.bytes.load(Ordering::Acquire),
            "reply_started":self.reply_started.load(Ordering::Acquire),
            "reply_eof":self.reply_eof.load(Ordering::Acquire),
            "transport_dropped":self.transport_dropped.load(Ordering::Acquire),
            "watchdog_expired":self.watchdog_expired.load(Ordering::Acquire)})
    }
}

#[derive(Default)]
struct StateControlState {
    next_id: u64,
    armed: Option<Arc<StateJob>>,
    current: Option<Arc<StateJob>>,
}
#[derive(Default)]
struct StateControls(Mutex<StateControlState>);
impl StateControls {
    fn apply(&self, control: StateControl) -> Result<Value, StatusCode> {
        let mut state = self.0.lock().unwrap();
        match control {
            StateControl::Arm {
                operation,
                hold_effect,
                hold_eof,
                reply,
                status,
            } => {
                if ![200, 500].contains(&status) {
                    return Err(StatusCode::BAD_REQUEST);
                }
                if state.armed.is_some()
                    || state.current.as_ref().is_some_and(|job| !job.terminal())
                {
                    return Err(StatusCode::CONFLICT);
                }
                state.next_id = state.next_id.checked_add(1).ok_or(StatusCode::CONFLICT)?;
                let id = state.next_id;
                state.armed = Some(Arc::new(StateJob {
                    id,
                    operation,
                    effect: StateGate::new(!hold_effect),
                    eof: StateGate::new(!hold_eof),
                    reply,
                    status,
                    physical_started: AtomicBool::new(false),
                    effect_started: AtomicBool::new(false),
                    effect_completed: AtomicBool::new(false),
                    physical_settled: AtomicBool::new(false),
                    bytes: AtomicU64::new(0),
                    reply_started: AtomicBool::new(false),
                    reply_eof: AtomicBool::new(false),
                    transport_dropped: AtomicBool::new(false),
                    watchdog_expired: AtomicBool::new(false),
                }));
                Ok(json!({"schema_version":1,"armed":true,"operation_id":id}))
            }
            StateControl::ReleaseEffect { operation_id }
            | StateControl::ReleaseEof { operation_id } => {
                let job = state
                    .armed
                    .iter()
                    .chain(state.current.iter())
                    .find(|job| job.id == operation_id)
                    .ok_or(StatusCode::NOT_FOUND)?;
                match control {
                    StateControl::ReleaseEffect { .. } => job.effect.release(),
                    _ => job.eof.release(),
                }
                Ok(json!({"schema_version":1,"released":true,"operation_id":operation_id}))
            }
        }
    }
    fn take(&self, operation: StateOperation) -> Result<Option<Arc<StateJob>>, StatusCode> {
        let mut state = self.0.lock().unwrap();
        if state.current.as_ref().is_some_and(|job| !job.terminal()) {
            return Err(StatusCode::CONFLICT);
        }
        if state
            .armed
            .as_ref()
            .is_some_and(|job| job.operation != operation)
        {
            return Ok(None);
        }
        let job = state.armed.take();
        if let Some(job) = &job {
            state.current = Some(job.clone());
        }
        Ok(job)
    }
    fn view(&self) -> Value {
        let state = self.0.lock().unwrap();
        json!({"armed":state.armed.as_ref().map(|job| job.view()),"current":state.current.as_ref().map(|job| job.view())})
    }
}

struct StateTransport(Arc<StateJob>);
impl Drop for StateTransport {
    fn drop(&mut self) {
        if !self.0.reply_eof.load(Ordering::Acquire) {
            self.0.transport_dropped.store(true, Ordering::Release);
        }
    }
}

async fn state_control(
    State(fixture): State<Arc<Fixture>>,
    Json(control): Json<StateControl>,
) -> Response {
    match fixture.state_controls.apply(control) {
        Ok(value) => Json(value).into_response(),
        Err(status) => status.into_response(),
    }
}
async fn state_observation(State(fixture): State<Arc<Fixture>>) -> Json<Value> {
    Json(
        json!({"schema_version":1,"pid":std::process::id(),"jobs":fixture.state_controls.view(),
        "inference_started":fixture.inference_started.load(Ordering::Acquire),
        "save_started":fixture.saves_started.load(Ordering::Acquire),
        "restore_started":fixture.restores_started.load(Ordering::Acquire),
        "inspection_started":fixture.inspections_started.load(Ordering::Acquire)}),
    )
}

fn slot_inventory(fixture: &Fixture) -> Value {
    Value::Array(
        (0..fixture.parallel)
            .map(|id| json!({"id":id,"n_ctx":fixture.context,"is_processing":false}))
            .collect(),
    )
}

async fn inspect_slots(State(fixture): State<Arc<Fixture>>) -> Response {
    fixture.inspections_started.fetch_add(1, Ordering::AcqRel);
    match fixture.state_controls.take(StateOperation::InspectSlots) {
        Ok(Some(job)) => {
            let observation = Observation {
                fixture: fixture.clone(),
                id: "slot-inspect".to_owned(),
                completed: false,
            };
            observation.record("started");
            controlled_slot_action(fixture, job, PathBuf::new(), String::new(), 0, observation)
                .await
        }
        Ok(None) => Json(slot_inventory(&fixture)).into_response(),
        Err(status) => status.into_response(),
    }
}

struct Observation {
    fixture: Arc<Fixture>,
    id: String,
    completed: bool,
}

impl Observation {
    fn record(&self, event: &str) {
        if let Some(path) = &self.fixture.events {
            record_event(path, &self.id, event);
        }
    }
}

fn record_event(path: &std::path::Path, id: &str, event: &str) {
    write_record(
        path,
        json!({"id":id,"event":event,"pid":std::process::id()}),
    );
}

fn write_record(path: &std::path::Path, value: Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let mut record = serde_json::to_vec(&value).unwrap();
    record.push(b'\n');
    file.write_all(&record).unwrap();
}

impl Drop for Observation {
    fn drop(&mut self) {
        self.record(if self.completed {
            "completed"
        } else {
            "cancelled"
        });
    }
}

async fn inference(
    State(fixture): State<Arc<Fixture>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let body_sha256 = format!("{:x}", Sha256::digest(&bytes));
    fixture.inference_started.fetch_add(1, Ordering::AcqRel);
    let mut observation = Observation {
        fixture,
        id: body["test_id"].as_str().unwrap_or("request").to_owned(),
        completed: false,
    };
    observation.record("started");
    if body["fixture_crash"] == true {
        std::process::exit(17);
    }
    let delay = body["fixture_delay_ms"]
        .as_u64()
        .unwrap_or(observation.fixture.inference_delay);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if observation.fixture.chatty_bytes > 0 {
        let bytes = observation.fixture.chatty_bytes;
        let id = observation.id.clone();
        tokio::task::spawn_blocking(move || chatter(&id, bytes))
            .await
            .unwrap();
    }
    let emulated = observation
        .fixture
        .prompt_cache
        .as_ref()
        .map(|slots| emulate_prompt(slots, &body));
    if let Some(status) = body["fixture_status"].as_u64() {
        observation.completed = true;
        return (
            StatusCode::from_u16(status as u16).unwrap(),
            Json(json!({"error": {"message": "fixture failure"}})),
        )
            .into_response();
    }
    if body["stream"] == true {
        let events = stream::unfold((0u32, observation), move |(index, mut observation)| {
            let emulated = emulated.clone();
            async move {
                if observation.fixture.finite_stream {
                    let event = match index {
                        0 | 1 => format!(
                            "data: {}\n\n",
                            json!({"id":"fixture-stream","model":"fixture","choices":[{"index":0,"delta":{"content":"λ "},"text":"λ ","finish_reason":null}]})
                        ),
                        2 => {
                            let mut event = json!({"id":"fixture-stream","choices":[{"index":0,"delta":{},"text":"","finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}});
                            if observation.fixture.report_timings {
                                event["timings"] = json!({"prompt_n":7,"prompt_ms":20.0,"prompt_per_second":350.0,"predicted_n":2,"predicted_ms":53.3,"predicted_per_second":37.5});
                            }
                            if let Some((usage, timings)) = &emulated {
                                event["usage"] = usage.clone();
                                event["timings"] = timings.clone();
                            }
                            format!("data: {event}\n\n")
                        }
                        3 => "data: [DONE]\n\n".to_owned(),
                        _ => {
                            observation.completed = true;
                            drop(observation);
                            return None;
                        }
                    };
                    return Some((Ok::<_, std::io::Error>(event), (index + 1, observation)));
                }
                if index >= 200 {
                    if observation.fixture.report_timings && index < 202 {
                        let event = if index == 200 {
                            format!(
                                "data: {}\n\n",
                                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":200,"total_tokens":207},"timings":{"prompt_n":7,"prompt_ms":20.0,"prompt_per_second":350.0,"predicted_n":200,"predicted_ms":3000.0,"predicted_per_second":66.7}})
                            )
                        } else {
                            "data: [DONE]\n\n".to_owned()
                        };
                        return Some((Ok::<_, std::io::Error>(event), (index + 1, observation)));
                    }
                    observation.completed = true;
                    drop(observation);
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
                Some((
                    Ok::<_, std::io::Error>(format!(
                        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{index} \"}}}}]}}\n\n"
                    )),
                    (index + 1, observation),
                ))
            }
        });
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(events),
        )
            .into_response();
    }
    observation.completed = true;
    *observation.fixture.slot.lock().unwrap() = body_sha256.as_bytes().to_vec();
    let restored = observation.fixture.restores.load(Ordering::Acquire);
    let mut reply = json!({"id": "fixture-completion", "object": "chat.completion", "model": body["model"],
        "usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9},
        "choices": [{"index": 0, "text":"Rust fixture reply.", "message": {"role": "assistant", "content": "Rust fixture reply. This is not model inference."}, "finish_reason": "stop"}],
        "received_authorization": headers.contains_key("authorization"),
        "received_body_sha256": body_sha256,
        "fixture_restores": restored,
        "manager_key_present": std::env::var_os("LEMONADE_API_KEY").is_some() || std::env::var_os("LEMONADE_ADMIN_API_KEY").is_some()
    });
    if observation.fixture.report_timings {
        reply["timings"] = json!({"prompt_n":7,"prompt_ms":10.0,"prompt_per_second":700.0,"predicted_n":2,"predicted_ms":47.06,"predicted_per_second":42.5});
    }
    if let Some((usage, timings)) = emulated {
        reply["usage"] = usage;
        reply["timings"] = timings;
    }
    Json(reply).into_response()
}

fn text_embedding(text: &str) -> Vec<f64> {
    let mut vector = vec![0.0; 8];
    for word in text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        let hash = word
            .to_lowercase()
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        let sign = if hash & 0x100 == 0 { 1.0 } else { -1.0 };
        vector[(hash % 8) as usize] += sign * (1.0 + (hash >> 16 & 0xff) as f64 / 255.0);
    }
    if vector.iter().all(|value| *value == 0.0) {
        vector[0] = 1.0;
    }
    vector
}

async fn embeddings(Json(body): Json<Value>) -> Json<Value> {
    let inputs: Vec<&str> = body["input"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if inputs.is_empty() {
        return Json(
            json!({"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.25,-0.5]}],"usage":{"prompt_tokens":2,"total_tokens":2}}),
        );
    }
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, text)| json!({"object":"embedding","index":index,"embedding":text_embedding(text)}))
        .collect();
    let tokens = inputs
        .iter()
        .map(|text| text.split_whitespace().count())
        .sum::<usize>();
    Json(
        json!({"object":"list","data":data,"usage":{"prompt_tokens":tokens,"total_tokens":tokens}}),
    )
}

const EMULATED_REPLY: &str = "Rust fixture reply. This is not model inference.";

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split_whitespace().map(str::to_owned)
}

fn content_words(value: &Value, tokens: &mut Vec<String>) {
    match value {
        Value::String(text) => tokens.extend(words(text)),
        Value::Array(parts) => {
            for part in parts {
                content_words(part.get("text").unwrap_or(part), tokens);
            }
        }
        Value::Null => {}
        other => tokens.push(other.to_string()),
    }
}

fn prompt_tokens(body: &Value) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut message = |message: &Value| {
        tokens.push(format!("<{}>", message["role"].as_str().unwrap_or("item")));
        content_words(&message["content"], &mut tokens);
    };
    if let Some(messages) = body["messages"].as_array() {
        messages.iter().for_each(&mut message);
    } else if let Some(items) = body["input"].as_array() {
        items.iter().for_each(&mut message);
    } else {
        content_words(
            if body["prompt"].is_null() {
                &body["input"]
            } else {
                &body["prompt"]
            },
            &mut tokens,
        );
    }
    tokens
}

fn emulate_prompt(slots: &Mutex<Vec<Vec<String>>>, body: &Value) -> (Value, Value) {
    let prompt = prompt_tokens(body);
    let mut slots = slots.lock().unwrap();
    let index = body["id_slot"]
        .as_u64()
        .map_or(0, |slot| slot as usize)
        .min(slots.len() - 1);
    let cached = if body["cache_prompt"] == false {
        0
    } else {
        slots[index]
            .iter()
            .zip(&prompt)
            .take_while(|(left, right)| left == right)
            .count()
    };
    let mut generated = vec!["<assistant>".to_owned()];
    generated.extend(words(EMULATED_REPLY));
    let processed = prompt.len() - cached;
    let output = generated.len() - 1;
    let total = prompt.len();
    slots[index] = prompt;
    slots[index].extend(generated);
    (
        json!({"prompt_tokens":total,"completion_tokens":output,"total_tokens":total + output,"prompt_tokens_details":{"cached_tokens":cached}}),
        json!({"prompt_n":processed,"cache_n":cached,"prompt_ms":processed as f64,"predicted_n":output,"predicted_ms":1.0,"predicted_per_second":1000.0,"id_slot":index}),
    )
}

async fn transcription(
    State(fixture): State<Arc<Fixture>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let mut fields = serde_json::Map::new();
    let mut received_file = Value::Null;
    while let Some(mut field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or("").to_owned();
        if name == "file" {
            let filename = field.file_name().unwrap_or("").to_owned();
            let content_type = field.content_type().unwrap_or("").to_owned();
            let mut hash = Sha256::new();
            let mut bytes = 0usize;
            while let Some(chunk) = field.chunk().await.unwrap() {
                hash.update(&chunk);
                bytes += chunk.len();
            }
            received_file = json!({"sha256":format!("{:x}",hash.finalize()),"bytes":bytes,"filename":filename,"content_type":content_type});
        } else {
            fields.insert(name, Value::String(field.text().await.unwrap()));
        }
    }
    let mut observation = Observation {
        fixture,
        id: "speech-request".into(),
        completed: false,
    };
    observation.record("started");
    if let Some(path) = observation.fixture.speech_physical_release.clone() {
        let fixture = observation.fixture.clone();
        // Native decoder work can outlive the HTTP future that dispatched it.
        let physical = tokio::task::spawn_blocking(move || {
            if let Some(events) = &fixture.events {
                record_event(events, "speech-physical", "started");
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while !path.try_exists().unwrap_or(false) {
                if std::time::Instant::now() >= deadline {
                    if let Some(events) = &fixture.events {
                        record_event(events, "speech-physical", "timed-out");
                        record_event(events, "speech-physical", "settled");
                    }
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(events) = &fixture.events {
                record_event(events, "speech-physical", "settled");
            }
            true
        });
        if !physical.await.unwrap_or(false) {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                "Fixture physical gate did not release",
            )
                .into_response();
        }
    }
    if let Some(path) = &observation.fixture.speech_release
        && !wait_for_release(path).await
    {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            "Fixture speech gate did not release",
        )
            .into_response();
    }
    if observation.fixture.inference_delay > 0 {
        tokio::time::sleep(Duration::from_millis(observation.fixture.inference_delay)).await;
    }
    observation.completed = true;
    if observation.fixture.speech_status != 200 {
        return (
            StatusCode::from_u16(observation.fixture.speech_status).unwrap(),
            "private worker error",
        )
            .into_response();
    }
    match observation.fixture.speech_mode.as_str() {
        "invalid-json" => {
            return ([("content-type", "application/json")], "{invalid").into_response();
        }
        "missing-text" => return Json(json!({"segments":[]})).into_response(),
        "error-object" => {
            return Json(json!({"error":{"message":"private worker error"}})).into_response();
        }
        "invalid-utf8" => return Bytes::from_static(&[0xff, 0xfe]).into_response(),
        "oversized" => return Bytes::from(vec![b'x'; 8 * 1024 * 1024 + 1]).into_response(),
        _ => {}
    }
    let format = fields
        .get("response_format")
        .and_then(Value::as_str)
        .unwrap_or("json");
    match format {
        "text" => return ([("content-type", "text/plain")], "Fixture speech λ\n").into_response(),
        "srt" => {
            return (
                [("content-type", "text/plain")],
                "1\n00:00:00,000 --> 00:00:01,000\nFixture speech λ\n",
            )
                .into_response();
        }
        "vtt" => {
            return (
                [("content-type", "text/plain")],
                "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nFixture speech λ\n",
            )
                .into_response();
        }
        _ => {}
    }
    Json(json!({"text":"Fixture speech λ", "received_fields":fields,
        "received_file":received_file,"received_model":fields.contains_key("model"),
        "received_authorization":headers.contains_key("authorization"),
        "manager_key_present":std::env::var_os("LEMONADE_API_KEY").is_some() || std::env::var_os("LEMONADE_ADMIN_API_KEY").is_some()
    })).into_response()
}

const FIXTURE_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\xcf\xc0\xf0\x1f\0\x05\0\x01\xff\x89\x99=\x1d\0\0\0\0IEND\xaeB`\x82";

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..4 {
            if index <= chunk.len() {
                output.push(ALPHABET[((word >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                output.push('=');
            }
        }
    }
    output
}

async fn media_delay(fixture: &Fixture, kind: &str) {
    if let Some(path) = &fixture.events {
        write_record(path, json!({"id":kind,"event":"started"}));
    }
    if fixture.inference_delay > 0 {
        tokio::time::sleep(Duration::from_millis(fixture.inference_delay)).await;
    }
    if let Some(path) = &fixture.events {
        write_record(path, json!({"id":kind,"event":"completed"}));
    }
}

async fn image_generation(
    State(fixture): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    media_delay(&fixture, "image-request").await;
    if fixture.speech_status != 200 {
        return (
            StatusCode::from_u16(fixture.speech_status).unwrap(),
            "private worker error",
        )
            .into_response();
    }
    if fixture.speech_mode == "invalid-image" {
        return Json(json!({"data":[{"b64_json":base64(b"not an image")}]})).into_response();
    }
    Json(
        json!({"created":0,"data":[{"b64_json":base64(FIXTURE_PNG)}],"output_format":"png",
        "received":body,"received_authorization":headers.contains_key("authorization")}),
    )
    .into_response()
}

async fn txt2img(State(fixture): State<Arc<Fixture>>, Json(body): Json<Value>) -> Response {
    media_delay(&fixture, "txt2img-request").await;
    Json(json!({"images":[base64(FIXTURE_PNG)],"parameters":body,"info":"{}"})).into_response()
}

async fn audio_speech(
    State(fixture): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    media_delay(&fixture, "tts-request").await;
    if fixture.speech_status != 200 {
        return (
            StatusCode::from_u16(fixture.speech_status).unwrap(),
            "private worker error",
        )
            .into_response();
    }
    let mut received = body.clone();
    received["received_authorization"] = json!(headers.contains_key("authorization"));
    received["espeak_data"] = json!(
        std::env::var("PIPER_ESPEAKNG_DATA_DIRECTORY")
            .ok()
            .filter(|directory| Some(directory) == std::env::var("ESPEAK_DATA_PATH").ok().as_ref())
            .map(|directory| std::path::Path::new(&directory).join("espeak-ng-data/phontab"))
            .filter(|phontab| phontab.is_file())
    );
    let data = serde_json::to_vec(&received).unwrap();
    match body["response_format"].as_str() {
        Some("mp3") => {
            let mut bytes = b"ID3\x04\0\0\0\0\0\0".to_vec();
            bytes.extend(data);
            ([("content-type", "audio/mpeg")], bytes).into_response()
        }
        _ => {
            let mut bytes = b"RIFF".to_vec();
            bytes.extend((36 + data.len() as u32).to_le_bytes());
            bytes.extend(b"WAVEfmt ");
            bytes.extend(16u32.to_le_bytes());
            bytes.extend(3u16.to_le_bytes());
            bytes.extend(1u16.to_le_bytes());
            bytes.extend(24000u32.to_le_bytes());
            bytes.extend(96000u32.to_le_bytes());
            bytes.extend(4u16.to_le_bytes());
            bytes.extend(32u16.to_le_bytes());
            bytes.extend(b"data");
            bytes.extend((data.len() as u32).to_le_bytes());
            bytes.extend(data);
            ([("content-type", "audio/wav")], bytes).into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct SlotAction {
    action: String,
}
async fn slot_action(
    State(fixture): State<Arc<Fixture>>,
    Path(slot): Path<u32>,
    Query(action): Query<SlotAction>,
    Json(body): Json<Value>,
) -> Response {
    if slot != 0 && (fixture.prompt_cache.is_none() || slot >= fixture.parallel) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(root) = &fixture.cache else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    let Some(name) = body["filename"].as_str().filter(|name| {
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    }) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = root.join(name);
    let action = match action.action.as_str() {
        "save" => {
            fixture.saves_started.fetch_add(1, Ordering::AcqRel);
            StateOperation::Save
        }
        "restore" => {
            fixture.restores_started.fetch_add(1, Ordering::AcqRel);
            StateOperation::Restore
        }
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let mut operation = Observation {
        fixture: fixture.clone(),
        id: match action {
            StateOperation::Save => "slot-save",
            StateOperation::Restore => "slot-restore",
            StateOperation::InspectSlots => unreachable!(),
        }
        .to_owned(),
        completed: false,
    };
    operation.record("started");
    match fixture.state_controls.take(action) {
        Ok(Some(job)) => {
            return controlled_slot_action(fixture, job, path, name.to_owned(), slot, operation)
                .await;
        }
        Ok(None) => {}
        Err(status) => return status.into_response(),
    }
    if fixture.slot_delay > 0 {
        tokio::time::sleep(Duration::from_millis(fixture.slot_delay)).await;
    }
    operation.completed = true;
    match slot_effect(&fixture, action, &path, name, slot, None) {
        Ok((ack, _)) => Json(ack).into_response(),
        Err(status) => status.into_response(),
    }
}

fn slot_effect(
    fixture: &Fixture,
    action: StateOperation,
    path: &std::path::Path,
    name: &str,
    slot: u32,
    maximum: Option<usize>,
) -> Result<(Value, u64), StatusCode> {
    match action {
        StateOperation::InspectSlots => Ok((slot_inventory(fixture), 0)),
        StateOperation::Save if fixture.prompt_cache.is_some() => {
            let tokens =
                fixture.prompt_cache.as_ref().unwrap().lock().unwrap()[slot as usize].clone();
            if tokens.is_empty() {
                return Err(StatusCode::CONFLICT);
            }
            let data = serde_json::to_vec(&tokens).unwrap();
            if maximum.is_some_and(|maximum| data.len() > maximum) {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            std::fs::write(path, &data).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok((
                json!({"id_slot":slot,"filename":name,"n_saved":tokens.len(),"n_written":data.len()}),
                data.len() as u64,
            ))
        }
        StateOperation::Restore if fixture.prompt_cache.is_some() => {
            let tokens: Vec<String> = std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .filter(|tokens: &Vec<String>| !tokens.is_empty())
                .ok_or(StatusCode::BAD_REQUEST)?;
            let bytes = serde_json::to_vec(&tokens).unwrap().len();
            let count = tokens.len();
            fixture.prompt_cache.as_ref().unwrap().lock().unwrap()[slot as usize] = tokens;
            fixture.restores.fetch_add(1, Ordering::AcqRel);
            Ok((
                json!({"id_slot":slot,"filename":name,"n_restored":count,"n_read":bytes}),
                bytes as u64,
            ))
        }
        StateOperation::Save => {
            let data = {
                let data = fixture.slot.lock().unwrap();
                if data.is_empty() {
                    return Err(StatusCode::CONFLICT);
                }
                if maximum.is_some_and(|maximum| data.len() > maximum) {
                    return Err(StatusCode::PAYLOAD_TOO_LARGE);
                }
                data.clone()
            };
            match std::fs::write(path, &data) {
                Ok(()) => Ok((
                    json!({"id_slot":slot,"filename":name,"n_saved":data.len(),"n_written":data.len()}),
                    data.len() as u64,
                )),
                Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
        StateOperation::Restore => match maximum.map_or_else(
            || std::fs::read(path),
            |maximum| {
                let mut bytes = Vec::new();
                let file = std::fs::File::open(path)?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || metadata.len() > maximum as u64 {
                    return Err(std::io::Error::other(
                        "controlled fixture slot exceeds its bound",
                    ));
                }
                file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > maximum {
                    return Err(std::io::Error::other(
                        "controlled fixture slot exceeds its bound",
                    ));
                }
                Ok(bytes)
            },
        ) {
            Ok(data) if !data.is_empty() => {
                let bytes = data.len();
                *fixture.slot.lock().unwrap() = data;
                fixture.restores.fetch_add(1, Ordering::AcqRel);
                Ok((
                    json!({"id_slot":slot,"filename":name,"n_restored":bytes,"n_read":bytes}),
                    bytes as u64,
                ))
            }
            _ => Err(StatusCode::BAD_REQUEST),
        },
    }
}

async fn controlled_slot_action(
    fixture: Arc<Fixture>,
    job: Arc<StateJob>,
    path: PathBuf,
    name: String,
    slot: u32,
    mut observation: Observation,
) -> Response {
    let transport = StateTransport(job.clone());
    let physical = job.clone();
    // HTTP future cancellation must not cancel the simulated backend file work.
    let result = tokio::task::spawn_blocking(move || {
        physical.physical_started.store(true, Ordering::Release);
        if fixture.slot_delay > 0 {
            std::thread::sleep(Duration::from_millis(fixture.slot_delay));
        }
        let result = if physical.effect.wait_blocking() {
            physical.effect_started.store(true, Ordering::Release);
            let result = slot_effect(
                &fixture,
                physical.operation,
                &path,
                &name,
                slot,
                Some(16 * 1024 * 1024),
            );
            if let Ok((_, bytes)) = &result {
                physical.bytes.store(*bytes, Ordering::Release);
                physical.effect_completed.store(true, Ordering::Release);
            }
            result
        } else {
            physical.watchdog_expired.store(true, Ordering::Release);
            Err(StatusCode::GATEWAY_TIMEOUT)
        };
        physical.physical_settled.store(true, Ordering::Release);
        result
    })
    .await;
    observation.completed = true;
    let (status, ack) = match result {
        Ok(Ok((ack, _))) => (StatusCode::from_u16(job.status).unwrap(), ack),
        Ok(Err(status)) => (status, json!({"error":"fixture state operation failed"})),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error":"fixture state executor failed"}),
        ),
    };
    let bytes = match job.reply {
        StateReply::Malformed => Bytes::from_static(b"{malformed fixture acknowledgement"),
        _ => Bytes::from(serde_json::to_vec(&ack).unwrap()),
    };
    let body = stream::unfold(
        (0u8, transport, bytes),
        |(phase, transport, bytes)| async move {
            if phase == 0 {
                transport.0.reply_started.store(true, Ordering::Release);
                return Some((
                    Ok::<_, std::io::Error>(bytes.clone()),
                    (1, transport, bytes),
                ));
            }
            if phase == 2 {
                return None;
            }
            if matches!(transport.0.reply, StateReply::Interrupted) {
                return Some((
                    Err(std::io::Error::other(
                        "controlled fixture state transport failure",
                    )),
                    (2, transport, bytes),
                ));
            }
            if !transport.0.eof.wait().await {
                transport.0.watchdog_expired.store(true, Ordering::Release);
                return Some((
                    Err(std::io::Error::other(
                        "controlled fixture state EOF watchdog",
                    )),
                    (2, transport, bytes),
                ));
            }
            transport.0.reply_eof.store(true, Ordering::Release);
            drop(transport);
            None
        },
    );
    (
        status,
        [("content-type", "application/json")],
        Body::from_stream(body),
    )
        .into_response()
}

async fn wait_for_release(path: &std::path::Path) -> bool {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !tokio::fs::try_exists(path).await.unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

fn chatter(id: &str, bytes: usize) {
    let line = format!("I fixture chatter {id} {}\n", "x".repeat(240));
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let mut written = 0;
    while written < bytes {
        if stdout.write_all(line.as_bytes()).is_err() || stderr.write_all(line.as_bytes()).is_err()
        {
            return;
        }
        written += line.len();
    }
    let _ = stdout.flush();
}

const RECORDED_ENVIRONMENT: [&str; 13] = [
    "FLM_MODEL_PATH",
    "XILINX_XRT",
    "LD_LIBRARY_PATH",
    "GGML_BACKEND_PATH",
    "GGML_XDNA_FAIL_CLOSED",
    "GGML_XDNA_EXPLICIT_PHASE",
    "GGML_XDNA_GPU_DEVICE",
    "GGML_XDNA_MAX_COPY_GB",
    "GGML_XDNA_MIN_BATCH",
    "HYBRID_GPU_DEVICE",
    "HYBRID_REQUIRE_NPU",
    "LEMONADE_API_KEY",
    "LEMONADE_ADMIN_API_KEY",
];

fn record_launch(path: &std::path::Path, args: &[String]) {
    use std::io::Write as _;
    let mut line = format!("argv:{}\n", args[1..].join(" "));
    for name in RECORDED_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            line.push_str(&format!("{name}={}\n", value.to_string_lossy()));
        }
    }
    let mirror = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .map(|directory| directory.join("lib/x86_64-linux-gnu/libxrt_core.so.2"))
    });
    if mirror.is_some_and(|mirror| !mirror.is_file()) {
        line.push_str("missing-mirror\n");
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Answers the FLM helper subcommands Juiced runs outside `serve`. Helpers start with a
/// cleared environment, so they record into the FLM store instead.
fn flm_helper(args: &[String]) -> i32 {
    let Some(store) = std::env::var_os("FLM_MODEL_PATH").map(PathBuf::from) else {
        return 2;
    };
    record_launch(&store.join("helpers.log"), args);
    let model = store.join("models/Gemma3-1B-NPU2");
    match args[1].as_str() {
        "list" => {
            let installed = model.join("config.json").is_file();
            println!(
                "{}",
                json!({"models":[
                    {"name":"gemma3:1b","installed":installed,"footprint":0.5,"label":[],"url":"https://huggingface.co/FastFlowLM/Gemma3-1B-NPU2/tree/main"},
                    {"name":"embed-gemma:300m","installed":true,"url":"https://huggingface.co/FastFlowLM/Embedding-Gemma-300M-NPU2"}
                ]})
            );
            0
        }
        "validate" => {
            println!(
                "{}",
                json!({"ready":true,"amd_device_found":true,"memlock_ok":true})
            );
            0
        }
        _ => {
            std::fs::create_dir_all(&model).unwrap();
            println!("[FLM]  Overall progress: 50.0% (1/2 files)");
            std::fs::write(
                model.join("config.json"),
                br#"{"max_position_embeddings":32768}"#,
            )
            .unwrap();
            println!("[FLM]  Overall progress: 100.0% (2/2 files)");
            0
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(path) = std::env::var_os("FIXTURE_LAUNCH_RECORD") {
        record_launch(std::path::Path::new(&path), &args);
    }
    if matches!(
        args.get(1).map(String::as_str),
        Some("list" | "validate" | "pull")
    ) {
        std::process::exit(flm_helper(&args));
    }
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    if let Some(path) = arg("--events") {
        write_record(
            std::path::Path::new(&path),
            json!({"id":"worker-startup","event":"started","pid":std::process::id(),"port":arg("--port").or_else(|| arg("--listen-port")).and_then(|port| port.parse::<u16>().ok()),"model_path":arg("--model-path"),"ws_port":arg("--ws-port"),"tcp_port":arg("--tcp-port"),"host":arg("--host")}),
        );
    }
    if arg("--mode").as_deref() == Some("exit") {
        std::process::exit(11);
    }
    if arg("--mode").as_deref() == Some("hang") {
        std::future::pending::<()>().await;
    }
    if let Some(delay) = arg("--startup-delay-ms") {
        tokio::time::sleep(Duration::from_millis(delay.parse().unwrap())).await;
    }
    if let Some(path) = arg("--startup-release-file")
        && !wait_for_release(std::path::Path::new(&path)).await
    {
        std::process::exit(12);
    }
    let port: u16 = arg("--port")
        .or_else(|| arg("--listen-port"))
        .unwrap()
        .parse()
        .unwrap();
    if let Some(marker) = arg("--startup-log") {
        println!("fixture stdout {marker}");
        eprintln!("W fixture stderr {marker}");
        eprintln!("{}", "L".repeat(10 * 1024));
    }
    let mut fixture = Fixture {
        #[cfg(target_os = "linux")]
        _identity_map: arg("--identity-map").map(|path| {
            IdentityMapping::open(std::path::Path::new(&path)).expect("open owned identity mapping")
        }),
        events: arg("--events").map(PathBuf::from),
        finite_stream: args.iter().any(|value| value == "--finite-stream"),
        report_timings: args.iter().any(|value| value == "--report-timings"),
        chatty_bytes: arg("--chatty-bytes")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        inference_delay: arg("--inference-delay-ms")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        parallel: arg("--parallel")
            .or_else(|| arg("-np"))
            .and_then(|value| value.parse().ok())
            .unwrap_or(2),
        context: arg("--ctx-size")
            .or_else(|| arg("-c"))
            .and_then(|value| value.parse().ok())
            .unwrap_or(512),
        slot_delay: arg("--slot-operation-delay-ms")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        speech_mode: arg("--speech-response-mode").unwrap_or_default(),
        speech_release: arg("--speech-release-file").map(PathBuf::from),
        speech_physical_release: arg("--speech-physical-release-file").map(PathBuf::from),
        speech_status: arg("--speech-status")
            .map(|s| s.parse().unwrap())
            .unwrap_or(200),
        cache: arg("--slot-save-path").map(PathBuf::from),
        slot: Mutex::new(Vec::new()),
        prompt_cache: None,
        restores: AtomicU64::new(0),
        state_controls: StateControls::default(),
        inference_started: AtomicU64::new(0),
        saves_started: AtomicU64::new(0),
        restores_started: AtomicU64::new(0),
        inspections_started: AtomicU64::new(0),
    };
    if args.iter().any(|value| value == "--emulate-prompt-cache") {
        fixture.prompt_cache = Some(Mutex::new(vec![Vec::new(); fixture.parallel as usize]));
    }
    let fixture = Arc::new(fixture);
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/tags", get(|| async { Json(json!({"models":[]})) }))
        .route("/v1/models", get(|| async { Json(json!({"object":"list","data":[{"id":"fixture-media","object":"model"}]})) }))
        .route("/v1/images/generations", post(image_generation))
        .route("/sdapi/v1/txt2img", post(txt2img))
        .route("/v1/audio/speech", post(audio_speech))
        .route("/inference", post(transcription).layer(DefaultBodyLimit::max(8 * 1024 * 1024)))
        .route("/v1/audio/transcriptions", post(transcription).layer(DefaultBodyLimit::max(8 * 1024 * 1024)))
        .route("/v1/chat/completions", post(inference))
        .route("/v1/completions", post(inference))
        .route("/v1/responses", post(inference))
        .route("/__fixture/state/control",post(state_control).layer(DefaultBodyLimit::max(2048)))
        .route("/__fixture/state",get(state_observation))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/messages/count_tokens", post(|| async { Json(json!({"input_tokens":7})) }))
        .route("/props", get(|State(fixture): State<Arc<Fixture>>| async move { Json(json!({"total_slots":fixture.parallel,"default_generation_settings":{"n_ctx":fixture.context},"build_info":"fixture"})) }))
        .route("/metrics", get(|| async { "# HELP llamacpp:spec_decode_num_draft_tokens_total Speculative: Total draft tokens generated\nllamacpp:spec_decode_num_draft_tokens_total 105\nllamacpp:spec_decode_num_accepted_tokens_total 48\nllamacpp:spec_decode_num_drafts_total 35\n" }))
        .route("/slots/{slot}", post(slot_action))
        .route("/slots",get(inspect_slots))
        .with_state(fixture);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
