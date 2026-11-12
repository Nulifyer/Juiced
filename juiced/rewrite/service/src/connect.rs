use crate::{
    ApiError,
    config::{Operation, Policy},
    extract::ApiJson,
    http::AppState,
    protocol_access::{HTTP_PROTOCOLS, Protocol},
};
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

const HEALTHY_WITHIN_MS: u64 = 60 * 60 * 1000;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Self-checks must not count as client traffic in the last-request record.
#[derive(Clone, Copy)]
pub(crate) struct Probe;

fn access_view(state: &AppState) -> Value {
    let auth = &state.auth;
    let mut keys = auth.environment_keys();
    keys.extend(auth.keys().views());
    json!({
        "schema_version": 1,
        "mode": if auth.enforcing() { "keys" } else { "trusted-lan" },
        "store_error": auth.keys().unavailable(),
        "keys": keys,
        "limits": {
            "keys": crate::api_keys::MAXIMUM_KEYS,
            "name_chars": crate::api_keys::MAXIMUM_NAME_CHARS,
            "expiry_days": crate::api_keys::MAXIMUM_EXPIRY_DAYS,
        },
    })
}

pub(crate) async fn access(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(access_view(&state)),
    )
}

pub(crate) async fn key_create(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::api_keys::Create>,
) -> Result<Response, ApiError> {
    let operator = state.auth.environment_operator();
    let keys = state.auth.keys().clone();
    let (key, secret) = tokio::spawn(async move { keys.create(request, operator).await })
        .await
        .map_err(|_| interrupted())??;
    let mut body = access_view(&state);
    body["created"] = json!({"key": key, "secret": secret});
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(body),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Revoke {
    id: String,
}

pub(crate) async fn key_revoke(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<Revoke>,
) -> Result<Json<Value>, ApiError> {
    let operator = state.auth.environment_operator();
    let keys = state.auth.keys().clone();
    tokio::spawn(async move { keys.revoke(&request.id, operator).await })
        .await
        .map_err(|_| interrupted())??;
    Ok(Json(access_view(&state)))
}

fn interrupted() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "access_change_interrupted",
        "The key change was interrupted. Refresh before retrying.",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProtocolChange {
    protocol: String,
    enabled: bool,
}

pub(crate) async fn protocol_set(
    State(state): State<AppState>,
    Extension(wyoming): Extension<Arc<crate::wyoming::Controller>>,
    ApiJson(change): ApiJson<ProtocolChange>,
) -> Result<Json<Value>, ApiError> {
    let protocol = Protocol::from_id(&change.protocol).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "unknown_protocol",
            "Name one of openai, anthropic, ollama, mcp, automatic1111, realtime or wyoming.",
        )
    })?;
    let manager = state.manager.clone();
    tokio::spawn(async move {
        if protocol == Protocol::Wyoming {
            let (mut settings, generation) = manager.wyoming_settings().await;
            if settings.enabled != change.enabled {
                settings.enabled = change.enabled;
                wyoming
                    .configure(crate::wyoming::Change {
                        settings,
                        expected_generation: generation,
                    })
                    .await?;
            }
            Ok(())
        } else {
            manager
                .set_protocol_enabled(protocol, change.enabled)
                .await
                .map(drop)
        }
    })
    .await
    .map_err(|_| interrupted())??;
    Ok(Json(
        json!({"protocol": protocol.id(), "enabled": change.enabled}),
    ))
}

fn served_by(protocol: Protocol, operations: &[Operation]) -> bool {
    operations.iter().any(|operation| match protocol {
        Protocol::Openai => *operation != Operation::TokenCount,
        Protocol::Anthropic => *operation == Operation::Chat,
        Protocol::Ollama | Protocol::Mcp => matches!(
            operation,
            Operation::Chat | Operation::Completion | Operation::Embeddings
        ),
        Protocol::Automatic1111 => *operation == Operation::ImageGeneration,
        Protocol::Realtime => *operation == Operation::Transcription,
        Protocol::Wyoming => matches!(operation, Operation::Transcription | Operation::Speech),
    })
}

fn probe_request(protocol: Protocol) -> (Method, &'static str, Option<&'static str>) {
    match protocol {
        Protocol::Openai => (Method::GET, "/v1/models", None),
        Protocol::Anthropic => (Method::POST, "/v1/messages", Some("{}")),
        Protocol::Ollama => (Method::GET, "/api/version", None),
        Protocol::Mcp => (
            Method::POST,
            "/mcp",
            Some(r#"{"jsonrpc":"2.0","id":"juiced-health","method":"ping"}"#),
        ),
        Protocol::Automatic1111 => (
            Method::POST,
            "/sdapi/v1/txt2img",
            Some(r#"{"juiced_health_probe":true}"#),
        ),
        Protocol::Realtime | Protocol::Wyoming => (Method::GET, "/v1/realtime", None),
    }
}

/// A probe that the handler rejects for its deliberately incomplete body proves the route,
/// authentication and protocol gate admitted it without starting inference.
async fn probe(router: Router, protocol: Protocol, headers: &HeaderMap) -> Value {
    let (method, path, body) = probe_request(protocol);
    let mut request = Request::builder().method(method.clone()).uri(path);
    for name in [header::AUTHORIZATION, "x-api-key".parse().unwrap()] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }
    if body.is_some() {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json");
    }
    let mut request = request
        .body(body.map_or_else(Body::empty, Body::from))
        .expect("static probe request");
    request.extensions_mut().insert(Probe);
    let started = std::time::Instant::now();
    let status = match tokio::time::timeout(PROBE_TIMEOUT, router.oneshot(request)).await {
        Ok(Ok(response)) => Some(response.status().as_u16()),
        _ => None,
    };
    let ok = status.is_some_and(|status| {
        status < 500
            && if body.is_some() || protocol == Protocol::Realtime {
                !matches!(status, 401 | 403 | 404 | 405)
            } else {
                status == 200
            }
    });
    json!({
        "ok": ok, "status": status, "route": format!("{method} {path}"),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    })
}

pub(crate) async fn health(
    State(state): State<AppState>,
    Extension(wyoming): Extension<Arc<crate::wyoming::Controller>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let router = state.probe.get().cloned().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "health_unavailable",
            "Protocol checks are not ready yet. Retry.",
        )
    })?;
    let catalog = state.manager.catalog().await;
    let gate = state.manager.protocol_gate();
    let now = crate::api_keys::now_ms();
    let count = |protocol| {
        catalog
            .iter()
            .filter(|model| {
                model.policy != Policy::Disabled
                    && model.downloaded
                    && served_by(protocol, &model.operations)
            })
            .count()
    };
    let checks = futures_util::future::join_all(HTTP_PROTOCOLS.map(|protocol| {
        let router = router.clone();
        let headers = &headers;
        async move {
            if gate.enabled(protocol) {
                probe(router, protocol, headers).await
            } else {
                Value::Null
            }
        }
    }))
    .await;
    let status = |enabled: bool, check: &Value, models: usize, last: Option<(u64, bool)>| {
        if !enabled {
            "disabled"
        } else if check["ok"] != true {
            "error"
        } else if models == 0 {
            "no_models"
        } else if last.is_some_and(|(at, ok)| ok && now.saturating_sub(at) <= HEALTHY_WITHIN_MS) {
            "healthy"
        } else {
            "idle"
        }
    };
    let inventory = crate::http::protocol_inventory();
    let describe = |id: &str, mut entry: Value| {
        if let Some(known) = inventory
            .as_array()
            .and_then(|all| all.iter().find(|known| known["id"] == id))
        {
            for field in ["support", "note", "operations"] {
                if let Some(value) = known.get(field) {
                    entry[field] = value.clone();
                }
            }
        }
        entry
    };
    let mut protocols: Vec<Value> = HTTP_PROTOCOLS
        .iter()
        .zip(checks)
        .map(|(&protocol, check)| {
            let enabled = gate.enabled(protocol);
            let models = count(protocol);
            let last = gate.last(protocol);
            describe(protocol.id(), json!({
                "id": protocol.id(), "title": protocol.title(), "enabled": enabled,
                "status": status(enabled, &check, models, last.map(|last| (last.at_unix_ms, last.status < 400))),
                "models": models, "check": check, "last_request": last,
            }))
        })
        .collect();
    let view = wyoming.view().await;
    let enabled = view["settings"]["enabled"] == true;
    let error = view["listener"]["error"]["message"].as_str();
    let check = if enabled {
        json!({"ok": view["listener"]["listening"] == true && error.is_none(), "status": null, "route": "TCP listener", "message": error})
    } else {
        Value::Null
    };
    let last = view["last_event"].as_object().and_then(|event| {
        Some((
            event.get("at_unix_ms")?.as_u64()?,
            event.get("outcome")?.as_str()?.to_owned(),
        ))
    });
    let models = count(Protocol::Wyoming);
    protocols.push(describe("wyoming", json!({
        "id": "wyoming", "title": "Wyoming", "enabled": enabled,
        "status": status(enabled, &check, models, last.as_ref().map(|(at, outcome)| (*at, outcome == "completed"))),
        "models": models, "check": check,
        "address": view["listener"]["address"], "listen": view["settings"]["listen"],
        "last_request": last.map(|(at, outcome)| json!({"at_unix_ms": at, "outcome": outcome})),
    })));
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"schema_version": 1, "checked_at_unix_ms": now, "protocols": protocols})),
    ))
}
