//! Barista's typed management tools.
//!
//! Each tool is an adapter onto native routes, replayed through the service router with the
//! caller's own credentials, so route authority, validation and revision checks stay
//! authoritative. Every tool answers with one bounded, redacted result envelope.

mod acquire;
mod configure;
mod explain;
mod inspect;
mod maintain;
mod verify;

use super::Permission;
use crate::{auth::RequestContext, http::AppState};
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, Method, Request, header},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::sync::Mutex;
use tower::ServiceExt;

const MAX_ROUTE_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Matches the console's per-result budget so nothing is cut again before the model sees it.
pub(super) const MAX_RESULT_BYTES: usize = 12 * 1024;
const MAX_STRING_BYTES: usize = 1500;
pub(super) const MODEL_ID_BYTES: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Group {
    Inspect,
    Acquire,
    Configure,
    Verify,
    Maintain,
    Explain,
}

impl Group {
    pub(super) const ALL: [Self; 6] = [
        Self::Inspect,
        Self::Acquire,
        Self::Configure,
        Self::Verify,
        Self::Maintain,
        Self::Explain,
    ];
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Inspect => "Inspect",
            Self::Acquire => "Acquire",
            Self::Configure => "Configure",
            Self::Verify => "Verify",
            Self::Maintain => "Maintain",
            Self::Explain => "Explain",
        }
    }
    pub(super) fn purpose(self) -> &'static str {
        match self {
            Self::Inspect => {
                "read status, hardware, models, engines, requests, logs, storage, cache and connections"
            }
            Self::Acquire => {
                "search Hugging Face, inspect repos, estimate fit, download, add models, install engines, wait for jobs"
            }
            Self::Configure => {
                "change policy, load state, model settings, cache, protocols, aliases, Wyoming and API keys"
            }
            Self::Verify => "run one real test request or the quick speed benchmark",
            Self::Maintain => {
                "remove models, files, sessions and engine builds; pause the service; restart a model"
            }
            Self::Explain => "procedures for whole jobs and the bundled docs",
        }
    }
}

/// Whether the native operation behind a tool covers everything it offers in this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Support {
    Working,
    /// Some targets have no native operation yet; the reason names them.
    Partial(&'static str),
}

pub(super) struct Tool {
    pub(super) name: &'static str,
    pub(super) group: Group,
    pub(super) title: &'static str,
    pub(super) description: &'static str,
    pub(super) read_only: bool,
    pub(super) destructive: bool,
    pub(super) idempotent: bool,
    pub(super) open_world: bool,
    /// Mutations that accept `preview: true` and then change nothing.
    pub(super) preview: bool,
    pub(super) support: Support,
    pub(super) input: fn() -> Value,
}

pub(super) fn catalog() -> impl Iterator<Item = &'static Tool> {
    inspect::TOOLS
        .iter()
        .chain(acquire::TOOLS)
        .chain(configure::TOOLS)
        .chain(verify::TOOLS)
        .chain(maintain::TOOLS)
        .chain(explain::TOOLS)
}

pub(super) fn tool(name: &str) -> Option<&'static Tool> {
    catalog().find(|tool| tool.name == name)
}

pub(super) fn schema(tool: &Tool) -> Value {
    let mut input = (tool.input)();
    if tool.preview {
        input["properties"]["preview"] = json!({"type":"boolean","default":false,"description":"true returns the effect and changes nothing."});
    }
    input
}

pub(super) fn describe(tool: &Tool, permission: Permission) -> Value {
    let mut annotations =
        json!({"title":tool.title,"readOnlyHint":tool.read_only,"openWorldHint":tool.open_world});
    if !tool.read_only {
        annotations["destructiveHint"] = json!(tool.destructive);
        annotations["idempotentHint"] = json!(tool.idempotent);
    }
    let confirm = !tool.read_only && permission == Permission::Ask;
    let (status, pending) = match tool.support {
        Support::Working => ("working", None),
        Support::Partial(reason) => ("partial", Some(reason)),
    };
    let mut entry = json!({
        "name":tool.name,"title":tool.title,"group":tool.group,"description":tool.description,
        "inputSchema":schema(tool),"annotations":annotations,"requires_confirmation":confirm,
        "previewable":tool.preview,"status":status,
    });
    if let Some(reason) = pending {
        entry["pending"] = json!(reason);
    }
    entry
}

pub(super) fn listed(context: &RequestContext, permission: Permission) -> Vec<Value> {
    if !context.operator() {
        return Vec::new();
    }
    catalog()
        .filter(|tool| tool.read_only || permission != Permission::ReadOnly)
        .map(|tool| describe(tool, permission))
        .collect()
}

#[derive(Debug)]
pub(super) enum Failure {
    InvalidParams(String),
    Route(Value),
}

impl Failure {
    pub(super) fn new(code: &str, message: impl Into<String>, status: u16) -> Self {
        Self::Route(json!({"error":{"code":code,"message":message.into(),"status":status}}))
    }
    fn error(&self) -> Value {
        match self {
            Self::InvalidParams(message) => {
                json!({"code":"invalid_arguments","message":message,"status":400})
            }
            Self::Route(value) => value["error"].clone(),
        }
    }
    pub(super) fn code(&self) -> &str {
        match self {
            Self::InvalidParams(_) => "invalid_arguments",
            Self::Route(value) => value["error"]["code"].as_str().unwrap_or(""),
        }
    }
}

pub(super) type Result<T> = std::result::Result<T, Failure>;

/// What a tool did, before the dispatcher wraps it in the envelope.
#[derive(Default)]
pub(super) struct Outcome {
    pub(super) summary: String,
    pub(super) data: Value,
    pub(super) changed: Vec<Value>,
    pub(super) job: Option<Value>,
    pub(super) next: Vec<&'static str>,
    pub(super) warnings: Vec<String>,
}

impl Outcome {
    pub(super) fn new(summary: impl Into<String>, data: Value) -> Self {
        Self {
            summary: summary.into(),
            data,
            ..Self::default()
        }
    }
    pub(super) fn next(mut self, tools: &[&'static str]) -> Self {
        self.next.extend_from_slice(tools);
        self
    }
    pub(super) fn warn(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
    pub(super) fn change(mut self, target: &str, field: &str, before: Value, after: Value) -> Self {
        self.changed
            .push(json!({"target":target,"field":field,"before":before,"after":after}));
        self
    }
    pub(super) fn job(mut self, job: Value) -> Self {
        self.job = Some(job);
        self
    }
}

pub(super) fn change(target: &str, field: &str, before: Value, after: Value) -> Value {
    json!({"target":target,"field":field,"before":before,"after":after})
}

/// Everything one dispatch needs: replayed routes, the barista settings it was authorized
/// against and whether this is a preview.
pub(super) struct Context<'a> {
    pub(super) state: &'a AppState,
    app: axum::Router,
    headers: &'a HeaderMap,
    pub(super) permission: Permission,
    pub(super) revision: u64,
    pub(super) assistant_model: Option<String>,
    pub(super) preview: bool,
    sources: Mutex<Vec<Value>>,
}

pub(super) struct Raw {
    pub(super) status: u16,
    pub(super) content_type: String,
    pub(super) bytes: Bytes,
}

impl Context<'_> {
    async fn send_raw(
        &self,
        method: Method,
        path: &str,
        content_type: Option<&str>,
        body: Option<Bytes>,
    ) -> Result<Raw> {
        let uri = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/v1/{path}")
        };
        let mut request = Request::builder().method(method.clone()).uri(&uri);
        for name in [
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-api-key"),
        ] {
            if let Some(value) = self.headers.get(&name) {
                request = request.header(name, value);
            }
        }
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let request = request
            .extension(crate::connect::Probe)
            .body(body.map_or_else(Body::empty, Body::from))
            .map_err(|_| {
                Failure::InvalidParams("The tool arguments cannot form a request.".into())
            })?;
        let unavailable = |message: &str| Failure::new("route_unavailable", message, 503);
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .map_err(|_| unavailable("The service route did not answer."))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let bytes = to_bytes(response.into_body(), MAX_ROUTE_RESPONSE_BYTES)
            .await
            .map_err(|_| unavailable("The service response exceeded the tool limit."))?;
        let route = uri.split('?').next().unwrap_or(&uri).to_owned();
        let mut source = json!({"route":format!("{method} {route}"),"status":status});
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            for key in [
                "revision",
                "generation",
                "configuration_generation",
                "artifact_generation",
            ] {
                if let Some(number) = value.get(key).filter(|value| value.is_u64()) {
                    source[key] = number.clone();
                }
            }
        }
        self.sources
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(source);
        Ok(Raw {
            status,
            content_type,
            bytes,
        })
    }

    pub(super) async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let has_body = body.is_some();
        let raw = self
            .send_raw(
                method,
                path,
                has_body.then_some("application/json"),
                body.map(|body| Bytes::from(body.to_string())),
            )
            .await?;
        decode(raw)
    }
    pub(super) async fn get(&self, path: &str) -> Result<Value> {
        self.send(Method::GET, path, None).await
    }
    pub(super) async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::POST, path, Some(body)).await
    }
    pub(super) async fn post_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Raw> {
        self.send_raw(
            Method::POST,
            path,
            Some(content_type),
            Some(Bytes::from(body)),
        )
        .await
    }
    pub(super) async fn post_raw(&self, path: &str, body: Value) -> Result<Raw> {
        self.send_raw(
            Method::POST,
            path,
            Some("application/json"),
            Some(Bytes::from(body.to_string())),
        )
        .await
    }

    pub(super) async fn catalog(&self) -> Result<Vec<Value>> {
        Ok(list(&self.get("juiced/catalog").await?, "data"))
    }
    pub(super) async fn model(&self, id: &str) -> Result<Value> {
        self.catalog()
            .await?
            .into_iter()
            .find(|model| model["id"] == id)
            .ok_or_else(|| {
                Failure::new(
                    "unknown_model",
                    format!("No registered model is named {id}. Use juiced_list_models."),
                    404,
                )
            })
    }
    /// The model's revision: the caller's value when given, so the native owner rejects stale
    /// plans, otherwise the current one.
    pub(super) fn revision(model: &Value, expected: Option<u64>) -> u64 {
        expected.unwrap_or_else(|| model["revision"].as_u64().unwrap_or(0))
    }
    pub(super) fn host(&self) -> Option<String> {
        let host = self.headers.get(header::HOST)?.to_str().ok()?;
        let scheme = self
            .headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .filter(|value| matches!(*value, "http" | "https"))
            .unwrap_or("http");
        Some(format!("{scheme}://{host}"))
    }
    pub(super) fn own_model(&self, id: &str) -> bool {
        self.assistant_model.as_deref() == Some(id)
    }
    /// Barista's settings may change while a server-side wait runs; stop as soon as they do.
    pub(super) async fn still_authorized(&self) -> bool {
        let settings = self.state.manager.barista_settings().await;
        settings.enabled && settings.revision == self.revision
    }
}

fn decode(raw: Raw) -> Result<Value> {
    let value: Value = serde_json::from_slice(&raw.bytes).unwrap_or(Value::Null);
    if (200..300).contains(&raw.status) {
        return Ok(value);
    }
    let mut error = match value.get("error") {
        Some(Value::Object(error)) => Value::Object(error.clone()),
        Some(Value::String(message)) => json!({"code":"request_failed","message":message}),
        _ => {
            json!({"code":"request_failed","message":format!("The service answered {}.", raw.status)})
        }
    };
    error["status"] = json!(raw.status);
    let mut failure = json!({"error":error});
    if let Some(extra) = value.as_object() {
        for (key, value) in extra.iter().filter(|(key, _)| *key != "error") {
            failure[key] = value.clone();
        }
    }
    Err(Failure::Route(failure))
}

pub(super) fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

pub(super) fn parse<T: DeserializeOwned>(args: Value) -> Result<T> {
    serde_json::from_value(args).map_err(|error| {
        let message = error.to_string();
        Failure::InvalidParams(format!(
            "Tool arguments do not match the input schema: {}",
            message.chars().take(200).collect::<String>()
        ))
    })
}

pub(super) fn bounded(value: &str, minimum: usize, maximum: usize, field: &str) -> Result<()> {
    if (minimum..=maximum).contains(&value.len()) && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(Failure::InvalidParams(format!(
            "{field} must be {minimum} to {maximum} bytes without control characters."
        )))
    }
}

pub(super) fn limit(value: Option<u32>, default: u32, maximum: u32, field: &str) -> Result<u32> {
    match value {
        None => Ok(default),
        Some(value) if (1..=maximum).contains(&value) => Ok(value),
        Some(_) => Err(Failure::InvalidParams(format!(
            "{field} must be 1 to {maximum}."
        ))),
    }
}

pub(super) fn pick(value: &Value, keys: &[&str]) -> Value {
    let mut output = Map::new();
    for key in keys {
        if let Some(field) = value.get(*key).filter(|field| !field.is_null()) {
            output.insert((*key).to_owned(), field.clone());
        }
    }
    Value::Object(output)
}

pub(super) fn list(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn present(value: &Value) -> Value {
    Value::Object(
        value
            .as_object()
            .map(|map| {
                map.iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// A page of a longer list with the count and a cursor for the next page.
pub(super) fn page(items: Vec<Value>, cursor: usize, size: usize) -> Value {
    let total = items.len();
    let start = cursor.min(total);
    let end = (start + size).min(total);
    let mut page = json!({"items":items[start..end],"total":total});
    if end < total {
        page["next_cursor"] = json!(end);
    }
    page
}

pub(super) fn model_id(value: &str) -> Result<()> {
    bounded(value, 1, MODEL_ID_BYTES as usize, "model")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelArgs {
    pub(super) model: String,
}

pub(super) fn model_arg(args: Value) -> Result<ModelArgs> {
    let args: ModelArgs = parse(args)?;
    model_id(&args.model)?;
    Ok(args)
}

pub(super) fn model_property() -> Value {
    json!({"type":"string","minLength":1,"maxLength":MODEL_ID_BYTES,"description":"Model id from juiced_list_models."})
}
pub(super) fn no_input() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
pub(super) fn model_input() -> Value {
    json!({"type":"object","required":["model"],"properties":{"model":model_property()},"additionalProperties":false})
}
pub(super) fn no_arguments(args: &Value) -> Result<()> {
    match args.as_object() {
        Some(map) if map.is_empty() => Ok(()),
        _ => Err(Failure::InvalidParams(
            "This tool accepts no arguments.".into(),
        )),
    }
}

pub(super) fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub(super) struct Dispatch<'a> {
    pub(super) state: &'a AppState,
    pub(super) headers: &'a HeaderMap,
    pub(super) permission: Permission,
    pub(super) revision: u64,
    pub(super) assistant_model: Option<String>,
    pub(super) preview: bool,
}

/// Runs one tool and returns its envelope. Authority is already checked by the caller.
pub(super) async fn call(
    dispatch: Dispatch<'_>,
    app: axum::Router,
    tool: &Tool,
    mut args: Value,
) -> Value {
    if tool.preview
        && let Some(map) = args.as_object_mut()
    {
        map.remove("preview");
    }
    let context = Context {
        state: dispatch.state,
        app,
        headers: dispatch.headers,
        permission: dispatch.permission,
        revision: dispatch.revision,
        assistant_model: dispatch.assistant_model,
        preview: dispatch.preview,
        sources: Mutex::default(),
    };
    let result = run(&context, tool, args).await;
    let sources = std::mem::take(
        &mut *context
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    );
    envelope(tool, context.preview, sources, result)
}

async fn run(context: &Context<'_>, tool: &Tool, args: Value) -> Result<Outcome> {
    let name = tool.name;
    let routed = match tool.group {
        Group::Inspect => inspect::run(context, name, args).await,
        Group::Acquire => acquire::run(context, name, args).await,
        Group::Configure => configure::run(context, name, args).await,
        Group::Verify => verify::run(context, name, args).await,
        Group::Maintain => maintain::run(context, name, args).await,
        Group::Explain => explain::run(context, name, args).await,
    };
    routed.unwrap_or_else(|| Err(Failure::InvalidParams("Unknown tool.".into())))
}

fn envelope(tool: &Tool, preview: bool, sources: Vec<Value>, result: Result<Outcome>) -> Value {
    let mut envelope = match result {
        Ok(outcome) => {
            let mut envelope = json!({
                "ok": true, "tool": tool.name, "summary": outcome.summary,
                "data": outcome.data, "source": sources,
            });
            if preview {
                envelope["preview"] = json!(true);
            }
            if !outcome.changed.is_empty() {
                envelope["changed"] = json!(outcome.changed);
            }
            if let Some(job) = outcome.job {
                envelope["job"] = job;
            }
            if !outcome.next.is_empty() {
                envelope["next"] = json!(outcome.next);
            }
            if !outcome.warnings.is_empty() {
                envelope["warnings"] = json!(outcome.warnings);
            }
            envelope
        }
        Err(failure) => {
            let error = failure.error();
            let message = error["message"]
                .as_str()
                .unwrap_or("The tool failed.")
                .to_owned();
            let mut envelope = json!({
                "ok": false, "tool": tool.name, "summary": message, "error": error, "source": sources,
            });
            if let Failure::Route(extra) = &failure
                && let Some(map) = extra.as_object()
            {
                let details: Map<String, Value> = map
                    .iter()
                    .filter(|(key, _)| *key != "error")
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                if !details.is_empty() {
                    envelope["data"] = Value::Object(details);
                }
            }
            let next: &[&str] = match failure.code() {
                "revision_conflict" => &["juiced_list_models"],
                "storage_revision_conflict" | "storage_item_refused" => &["juiced_storage"],
                "storage_revision_required" => &["juiced_delete_files"],
                "unknown_model" => &["juiced_list_models"],
                "engine_not_installed" => &["juiced_list_engines", "juiced_install_engine"],
                _ => &[],
            };
            if !next.is_empty() {
                envelope["next"] = json!(next);
            }
            envelope
        }
    };
    redact(&mut envelope);
    bound(&mut envelope);
    envelope
}

const SECRET_KEYS: &[&str] = &[
    "secret",
    "api_key",
    "apikey",
    "authorization",
    "password",
    "token",
    "hf_token",
    "access_token",
];

fn secret_like(text: &str) -> bool {
    let generated = text.len() == 50
        && text.starts_with("juiced_")
        && text[7..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    generated || text.starts_with("Bearer ") || text.starts_with("hf_") && text.len() > 30
}

/// Secrets never reach the model, whatever a route returns.
pub(super) fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if field.is_string() && SECRET_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                    *field = json!("[redacted]");
                } else {
                    redact(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        Value::String(text) if secret_like(text) => *text = "[redacted]".into(),
        _ => {}
    }
}

fn clip_strings(value: &mut Value) {
    match value {
        Value::Object(map) => map.values_mut().for_each(clip_strings),
        Value::Array(items) => items.iter_mut().for_each(clip_strings),
        Value::String(text) if text.len() > MAX_STRING_BYTES => {
            let mut end = MAX_STRING_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str(" [truncated]");
        }
        _ => {}
    }
}

/// JSON pointer to the largest list with more than one entry.
fn largest_array(value: &Value) -> Option<String> {
    fn walk(value: &Value, path: String, best: &mut Option<(String, usize)>) {
        match value {
            Value::Array(items) => {
                let bytes = serde_json::to_vec(value).map_or(0, |bytes| bytes.len());
                if items.len() > 1 && best.as_ref().is_none_or(|(_, known)| bytes > *known) {
                    *best = Some((path.clone(), bytes));
                }
                for (index, item) in items.iter().enumerate() {
                    walk(item, format!("{path}/{index}"), best);
                }
            }
            Value::Object(map) => {
                for (key, field) in map {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    walk(field, format!("{path}/{key}"), best);
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    walk(value, String::new(), &mut best);
    best.map(|(path, _)| path)
}

/// Keeps the envelope within the result budget by clipping long strings, then halving the
/// largest lists, and says so in a warning so the model narrows its request instead.
pub(super) fn bound(envelope: &mut Value) {
    let size = |value: &Value| serde_json::to_vec(value).map_or(0, |bytes| bytes.len());
    if size(envelope) <= MAX_RESULT_BYTES {
        return;
    }
    clip_strings(&mut envelope["data"]);
    let mut halved = false;
    for _ in 0..24 {
        if size(envelope) <= MAX_RESULT_BYTES {
            break;
        }
        let Some(path) = largest_array(&envelope["data"]) else {
            break;
        };
        let Some(Value::Array(items)) = envelope["data"].pointer_mut(&path) else {
            break;
        };
        let keep = items.len() / 2;
        items.truncate(keep.max(1));
        halved = true;
    }
    if size(envelope) > MAX_RESULT_BYTES {
        envelope["data"] = json!({"omitted":true});
    }
    let warning = if halved {
        "Output shortened to fit the tool limit: lists show only their first entries. Use a filter, limit or cursor for the rest."
    } else {
        "Output shortened to fit the tool limit."
    };
    match envelope["warnings"].as_array_mut() {
        Some(warnings) => warnings.push(json!(warning)),
        None => envelope["warnings"] = json!([warning]),
    }
    if size(envelope) > MAX_RESULT_BYTES {
        envelope["source"] = json!([]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use std::collections::BTreeSet;

    fn context(role: Role) -> RequestContext {
        RequestContext {
            principal: "test",
            role,
            credential_epoch: "test".into(),
        }
    }

    #[test]
    fn catalog_schemas_are_closed_objects_with_unique_names() {
        let mut names = BTreeSet::new();
        for tool in catalog() {
            assert!(names.insert(tool.name), "duplicate {}", tool.name);
            assert!(tool.name.starts_with("juiced_"), "{}", tool.name);
            assert!(
                tool.description.len() <= 320,
                "{} description is long",
                tool.name
            );
            let input = schema(tool);
            assert_eq!(input["type"], "object", "{}", tool.name);
            assert_eq!(input["additionalProperties"], false, "{}", tool.name);
            assert_eq!(
                input["properties"].get("preview").is_some(),
                tool.preview,
                "{}",
                tool.name
            );
            if tool.preview {
                assert!(!tool.read_only, "{} previews but is read only", tool.name);
            }
            if tool.read_only {
                assert!(!tool.destructive, "{}", tool.name);
            }
            for required in input["required"].as_array().into_iter().flatten() {
                assert!(
                    input["properties"]
                        .get(required.as_str().unwrap())
                        .is_some(),
                    "{} requires an undeclared field",
                    tool.name
                );
            }
        }
        for group in Group::ALL {
            assert!(
                catalog().any(|tool| tool.group == group),
                "{group:?} is empty"
            );
        }
        for excluded in [
            "shell",
            "exec",
            "file_write",
            "convert",
            "quantize",
            "barista",
            "permission",
        ] {
            assert!(
                !names.iter().any(|name| name.contains(excluded)),
                "{excluded} must not be a tool"
            );
        }
    }

    #[test]
    fn listing_follows_operator_authority_and_permission() {
        assert!(listed(&context(Role::Inference), Permission::Full).is_empty());
        assert!(listed(&context(Role::Read), Permission::Full).is_empty());
        let total = catalog().count();
        for role in [Role::Operator, Role::TrustedLan] {
            let full = listed(&context(role), Permission::Full);
            assert_eq!(full.len(), total);
            assert!(
                full.iter()
                    .all(|tool| tool["requires_confirmation"] == false)
            );
            let ask = listed(&context(role), Permission::Ask);
            assert!(ask.iter().all(|tool| tool["requires_confirmation"]
                == !tool["annotations"]["readOnlyHint"].as_bool().unwrap()));
            let read = listed(&context(role), Permission::ReadOnly);
            assert_eq!(read.len(), catalog().filter(|tool| tool.read_only).count());
            assert!(
                read.iter()
                    .all(|tool| tool["annotations"]["readOnlyHint"] == true)
            );
        }
        let pending: Vec<_> = listed(&context(Role::Operator), Permission::Full)
            .into_iter()
            .filter(|tool| tool["status"] != "working")
            .collect();
        assert!(pending.iter().all(|tool| tool["pending"].is_string()));
    }

    #[test]
    fn redaction_removes_secrets_but_keeps_counts_and_tool_names() {
        let secret = format!("juiced_{}", "A".repeat(43));
        let mut value = json!({
            "secret": "abc", "token": "x", "context_tokens": 4096, "input_tokens": 7,
            "nested": [{"api_key": "k"}, secret.clone(), "juiced_list_models"],
            "authorization": "Bearer abc", "note": format!("Bearer {secret}"),
        });
        redact(&mut value);
        assert_eq!(value["secret"], "[redacted]");
        assert_eq!(value["token"], "[redacted]");
        assert_eq!(value["context_tokens"], 4096);
        assert_eq!(value["nested"][0]["api_key"], "[redacted]");
        assert_eq!(value["nested"][1], "[redacted]");
        assert_eq!(value["nested"][2], "juiced_list_models");
        assert_eq!(value["authorization"], "[redacted]");
        assert_eq!(value["note"], "[redacted]");
        assert!(!value.to_string().contains(&secret));
    }

    #[test]
    fn oversized_output_is_bounded_with_a_warning() {
        let rows: Vec<Value> = (0..2000)
            .map(|index| json!({"id":index,"text":"x".repeat(40)}))
            .collect();
        let mut envelope = json!({"ok":true,"summary":"s","data":{"rows":rows,"long":"y".repeat(5000)},"source":[]});
        bound(&mut envelope);
        assert!(serde_json::to_vec(&envelope).unwrap().len() <= MAX_RESULT_BYTES);
        assert!(
            envelope["warnings"][0]
                .as_str()
                .unwrap()
                .starts_with("Output shortened")
        );
        assert!(envelope["data"]["rows"].as_array().unwrap().len() < 2000);
        assert!(
            envelope["data"]["long"]
                .as_str()
                .unwrap()
                .ends_with("[truncated]")
        );
        let mut small = json!({"ok":true,"summary":"s","data":{"rows":[1,2,3]},"source":[]});
        bound(&mut small);
        assert!(small.get("warnings").is_none());
    }

    #[test]
    fn pages_count_and_continue() {
        let items: Vec<Value> = (0..25).map(|index| json!(index)).collect();
        let first = page(items.clone(), 0, 10);
        assert_eq!(first["total"], 25);
        assert_eq!(first["next_cursor"], 10);
        let last = page(items, 20, 10);
        assert_eq!(last["items"].as_array().unwrap().len(), 5);
        assert!(last.get("next_cursor").is_none());
    }

    #[test]
    fn every_engine_says_what_it_runs_and_resolves_by_any_spelling() {
        for engine in crate::engines::catalog::Engine::all() {
            assert!(
                !inspect::engine_use(engine.name()).is_empty(),
                "{} needs an engine_use line",
                engine.name()
            );
            assert_eq!(inspect::engine_name(engine.name()), Some(engine.name()));
            assert_eq!(inspect::engine_name(engine.recipe()), Some(engine.name()));
        }
        assert_eq!(inspect::engine_name("vLLM"), Some("vllm"));
        assert_eq!(inspect::engine_name("shell"), None);
    }

    #[test]
    fn path_segments_and_queries_are_percent_encoded() {
        assert_eq!(encode("fixture.chat-q4"), "fixture.chat-q4");
        assert_eq!(encode("a/b c?&"), "a%2Fb%20c%3F%26");
    }
}
