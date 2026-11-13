use crate::{
    ApiError,
    auth::RequestContext,
    config::Operation,
    extract::{ApiBytes, ApiQuery},
    http::{AppState, forward_prioritized, request_priority},
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const SLOT_FIELDS: [&str; 5] = ["id", "id_task", "n_ctx", "speculative", "is_processing"];

fn invalid(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn bounded_model(model: Option<&str>) -> Result<Option<&str>, ApiError> {
    match model {
        Some(model)
            if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) =>
        {
            Err(invalid(
                "invalid_model",
                "Name one bounded registered model ID.",
            ))
        }
        model => Ok(model),
    }
}

fn object(body: &[u8]) -> Result<Map<String, Value>, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(invalid("invalid_request", "Provide a JSON object body.")),
    }
}

fn body_model(body: &Map<String, Value>) -> Result<Option<&str>, ApiError> {
    match body.get("model") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(model)) => bounded_model(Some(model)),
        Some(_) => Err(invalid(
            "invalid_model",
            "Name one bounded registered model ID.",
        )),
    }
}

fn no_store(value: Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

fn engine_reply(
    status: StatusCode,
    value: Value,
    operation: &'static str,
) -> Result<Response, ApiError> {
    if status.is_success() {
        return Ok(no_store(value));
    }
    Err(if status.is_client_error() {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "engine_rejected",
            format!("The engine rejected this {operation} request."),
        )
    } else {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "engine_failed",
            format!("The engine failed this {operation} request."),
        )
    })
}

pub(crate) async fn rerank(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let request = object(&body)?;
    if !request.get("query").is_some_and(Value::is_string)
        || !request.get("documents").is_some_and(Value::is_array)
    {
        return Err(invalid(
            "invalid_request",
            "Provide model, a query string and a documents array.",
        ));
    }
    let priority = request_priority(&context, &headers)?;
    forward_prioritized(state.manager, body, Operation::Rerank, priority, None, None).await
}

pub(crate) async fn tokenize(
    State(state): State<AppState>,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let request = object(&body)?;
    if !request.get("content").is_some_and(Value::is_string) {
        return Err(invalid(
            "content_required",
            "'content' parameter is required",
        ));
    }
    let mut upstream = Map::new();
    for field in ["content", "add_special", "parse_special", "with_pieces"] {
        if let Some(value) = request.get(field) {
            if field != "content" && !value.is_boolean() {
                return Err(invalid(
                    "invalid_request",
                    "add_special, parse_special and with_pieces are booleans.",
                ));
            }
            upstream.insert(field.to_owned(), value.clone());
        }
    }
    let target = state.manager.llama_target(body_model(&request)?).await?;
    let (status, value) = state
        .manager
        .llama_exchange(
            &target,
            reqwest::Method::POST,
            "/tokenize",
            Some(&Value::Object(upstream)),
        )
        .await?;
    engine_reply(status, value, "tokenize")
}

pub(crate) async fn detokenize(
    State(state): State<AppState>,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let request = object(&body)?;
    let tokens = request
        .get("tokens")
        .and_then(Value::as_array)
        .filter(|tokens| tokens.iter().all(|token| token.is_u64() || token.is_i64()))
        .ok_or_else(|| invalid("tokens_required", "'tokens' must be an array of token IDs"))?;
    let target = state.manager.llama_target(body_model(&request)?).await?;
    let (status, value) = state
        .manager
        .llama_exchange(
            &target,
            reqwest::Method::POST,
            "/detokenize",
            Some(&json!({"tokens": tokens})),
        )
        .await?;
    engine_reply(status, value, "detokenize")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelQuery {
    model: Option<String>,
}

/// Slot listings keep only progress fields: sampling parameters and prompts can carry
/// another caller's content.
fn slot_view(slot: &Value) -> Value {
    let mut view = Map::new();
    for field in SLOT_FIELDS {
        if let Some(value) = slot.get(field) {
            view.insert(field.to_owned(), value.clone());
        }
    }
    let numeric = |value: &Value| -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .filter(|(_, value)| value.is_number() || value.is_boolean())
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
            _ => Value::Null,
        }
    };
    if let Some(next) = slot.get("next_token") {
        view.insert(
            "next_token".to_owned(),
            match next {
                Value::Array(items) => Value::Array(items.iter().map(numeric).collect()),
                other => numeric(other),
            },
        );
    }
    let processing = slot["is_processing"].as_bool().unwrap_or(false);
    view.insert(
        "state".to_owned(),
        json!(if processing { "processing" } else { "idle" }),
    );
    Value::Object(view)
}

pub(crate) async fn slots(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<ModelQuery>,
) -> Result<Response, ApiError> {
    let target = state
        .manager
        .llama_target(bounded_model(query.model.as_deref())?)
        .await?;
    let (status, value) = state
        .manager
        .llama_exchange(&target, reqwest::Method::GET, "/slots", None)
        .await?;
    if status == StatusCode::NOT_IMPLEMENTED {
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "slots_unavailable",
            "This worker was started without its slots endpoint.",
        ));
    }
    let slots = value
        .as_array()
        .filter(|_| status.is_success())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "engine_failed",
                "The engine did not list its slots.",
            )
        })?;
    Ok(no_store(Value::Array(
        slots.iter().map(slot_view).collect(),
    )))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlotQuery {
    action: Option<String>,
    model: Option<String>,
}

pub(crate) async fn slot_action(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(slot): Path<String>,
    ApiQuery(query): ApiQuery<SlotQuery>,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let slot: u32 = slot
        .parse()
        .map_err(|_| invalid("invalid_slot", "Use a numeric slot ID."))?;
    let action = query.action.as_deref().ok_or_else(|| {
        invalid(
            "action_required",
            "POST /v1/slots/{id} requires action query parameter (e.g., ?action=erase)",
        )
    })?;
    if !matches!(action, "save" | "restore" | "erase") {
        return Err(invalid(
            "invalid_action",
            "Supported actions: erase, save, restore",
        ));
    }
    let request = object(&body)?;
    let model = match bounded_model(query.model.as_deref())? {
        Some(model) => Some(model),
        None => body_model(&request)?,
    };
    let target = state.manager.llama_target(model).await?;
    if action == "erase" {
        return Ok(no_store(state.manager.slot_erase(&target, slot).await?));
    }
    let filename = request
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            invalid(
                "filename_required",
                "Provide the slot filename in the JSON body.",
            )
        })?
        .to_owned();
    let reply = if action == "save" {
        let maximum_bytes = match request.get("maximum_bytes") {
            None => None,
            Some(value) => Some(value.as_u64().filter(|bytes| *bytes > 0).ok_or_else(|| {
                invalid("invalid_request", "maximum_bytes is a positive byte count.")
            })?),
        };
        state
            .manager
            .slot_save(&target, slot, filename, maximum_bytes, context)
            .await?
    } else {
        state
            .manager
            .slot_restore(&target, slot, filename, context)
            .await?
    };
    reply.into_response().await
}

pub(crate) async fn stats(State(state): State<AppState>) -> Response {
    no_store(state.manager.llama_stats())
}

pub(crate) async fn audio_voices(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<ModelQuery>,
) -> Result<Response, ApiError> {
    let model = bounded_model(query.model.as_deref())?
        .ok_or_else(|| invalid("model_required", "Specify a speech model"))?;
    Ok(no_store(state.manager.audio_voices(model).await?))
}

/// Liveness answers before authentication so orchestrators can probe a keyed service.
pub(crate) fn public() -> Router {
    Router::new().route("/live", get(|| async { Json(json!({"status": "ok"})) }))
}
