//! `POST /v1/classify` (native v0.9.2): encoder text classification on an `ort-server`
//! worker, whose own contract is `POST /classify {text}` -> `{labels: {label: score}}`.

use crate::{
    ApiError,
    auth::RequestContext,
    config::Operation,
    extract::ApiBytes,
    http::{AppState, forward_prioritized, request_priority},
};
use axum::{
    Extension, Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;

fn rejected(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": {"message": message, "type": "invalid_request_error"}})),
    )
        .into_response()
}

/// The worker body for a valid request, or the native validation message.
pub fn upstream(request: &Map<String, Value>) -> Result<(String, Value), &'static str> {
    let text = request.get("text").or_else(|| request.get("input"));
    if request.get("model").is_some_and(|model| !model.is_string()) {
        return Err("'model' must be a string");
    }
    let Some(text) = text else {
        return Err("Missing 'input' (or 'text') string in classify request");
    };
    let Some(text) = text.as_str() else {
        return Err(if request.contains_key("text") {
            "'text' must be a string"
        } else {
            "'input' must be a string"
        });
    };
    if text.trim().is_empty() {
        return Err("input text must not be empty");
    }
    if let Some(top_k) = request.get("top_k")
        && !top_k
            .as_u64()
            .is_some_and(|top_k| (1..=1_000_000).contains(&top_k))
    {
        return Err("'top_k' must be a positive integer");
    }
    let Some(model) = request.get("model").and_then(Value::as_str) else {
        return Err(
            "No 'model' specified and no single classification model is loaded (load one, or name it in the request)",
        );
    };
    Ok((model.to_owned(), json!({"model": model, "text": text})))
}

/// Native response shape, keeping the `top_k` highest scores when asked.
pub fn reply(model: &str, worker: &Value, top_k: Option<u64>) -> Option<Value> {
    let labels = worker.get("labels")?.as_object()?;
    let mut scored: Vec<(&String, f64)> = labels
        .iter()
        .map(|(label, score)| Some((label, score.as_f64()?)))
        .collect::<Option<_>>()?;
    scored.sort_by(|left, right| right.1.total_cmp(&left.1));
    if let Some(top_k) = top_k {
        scored.truncate(usize::try_from(top_k).unwrap_or(usize::MAX));
    }
    let labels: Map<String, Value> = scored
        .into_iter()
        .map(|(label, score)| (label.clone(), json!(score)))
        .collect();
    Some(json!({"object": "classification", "model": model, "labels": labels}))
}

pub(crate) async fn classify(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let Ok(Value::Object(request)) = serde_json::from_slice::<Value>(&body) else {
        return Ok(rejected("Invalid JSON in request body"));
    };
    let (model, worker_body) = match upstream(&request) {
        Ok(found) => found,
        Err(message) => return Ok(rejected(message)),
    };
    let top_k = request.get("top_k").and_then(Value::as_u64);
    let priority = request_priority(&context, &headers)?;
    let response = forward_prioritized(
        state.manager,
        Bytes::from(worker_body.to_string()),
        Operation::Classify,
        priority,
        None,
        None,
    )
    .await?;
    if !response.status().is_success() {
        return Ok(response);
    }
    let bytes = axum::body::to_bytes(response.into_body(), MAX_REPLY_BYTES)
        .await
        .map_err(|_| engine_reply())?;
    let worker: Value = serde_json::from_slice(&bytes).map_err(|_| engine_reply())?;
    reply(&model, &worker, top_k)
        .map(|value| Json(value).into_response())
        .ok_or_else(engine_reply)
}

fn engine_reply() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "engine_failed",
        "The classifier returned no label scores.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn requests_validate_like_native_and_accept_text_or_input() {
        let (model, body) = upstream(&object(json!({"model": "m", "input": "hello"}))).unwrap();
        assert_eq!(model, "m");
        assert_eq!(body, json!({"model": "m", "text": "hello"}));
        assert_eq!(
            upstream(&object(json!({"model": "m", "text": "hi"})))
                .unwrap()
                .1["text"],
            "hi"
        );
        for (request, message) in [
            (
                json!({"model": "m"}),
                "Missing 'input' (or 'text') string in classify request",
            ),
            (
                json!({"model": "m", "input": 1}),
                "'input' must be a string",
            ),
            (
                json!({"model": "m", "input": "  "}),
                "input text must not be empty",
            ),
            (
                json!({"model": "m", "input": "x", "top_k": 0}),
                "'top_k' must be a positive integer",
            ),
            (
                json!({"model": 3, "input": "x"}),
                "'model' must be a string",
            ),
        ] {
            assert_eq!(upstream(&object(request)).unwrap_err(), message);
        }
    }

    #[test]
    fn replies_sort_and_truncate_labels() {
        let worker = json!({"labels": {"LABEL_0": 0.1, "LABEL_1": 0.8, "LABEL_2": 0.1}});
        let value = reply("m", &worker, Some(1)).unwrap();
        assert_eq!(
            value,
            json!({"object": "classification", "model": "m", "labels": {"LABEL_1": 0.8}})
        );
        assert!(reply("m", &json!({"labels": {"a": "x"}}), None).is_none());
    }
}
