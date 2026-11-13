//! Client discovery routes: `/health`, `/models`, `/juiced/catalog`, `/juiced/capabilities`
//! and `/.well-known/juiced.json`, in the field names native Juiced v0.9 used.

use crate::{
    ApiError,
    config::Policy,
    model_discovery::SCHEMA_VERSION,
    supervisor::{Discovered, Manager, catalog_revision, loaded_entry, model_type},
};
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub const IMPLEMENTATION: &str = "rust-experiment";
const MODEL_TYPES: [&str; 7] = [
    "llm",
    "embedding",
    "reranking",
    "transcription",
    "image",
    "tts",
    "classification",
];

fn listed(model: &Discovered) -> bool {
    model.view.policy != Policy::Disabled && model.view.downloaded
}

fn aliases(manager: &Manager, models: &[Discovered]) -> Vec<(String, String)> {
    let views: Vec<_> = models.iter().map(|model| model.view.clone()).collect();
    manager
        .aliased_views(&views)
        .into_iter()
        .map(|(alias, view)| (alias, view.id))
        .collect()
}

fn matches(headers: &HeaderMap, tag: &str) -> bool {
    let strip = |value: &str| value.trim().trim_start_matches("W/").to_owned();
    let wanted = strip(tag);
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|candidate| candidate.trim() == "*" || strip(candidate) == wanted)
}

/// Weak validators: the body also carries live counters that do not change the revision.
fn conditional(headers: &HeaderMap, revision: &str, variant: &str, body: Value) -> Response {
    let tag = format!("W/\"{revision}.{variant}\"");
    let value = HeaderValue::from_str(&tag).expect("hex entity tag");
    let mut response = if matches(headers, &tag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(body).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::ETAG, value);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::VARY,
        HeaderValue::from_static("authorization, x-api-key, anthropic-version"),
    );
    response
}

pub async fn health(manager: &Arc<Manager>) -> Value {
    let models = manager.discovered(false).await;
    let loaded: Vec<&Discovered> = models.iter().filter(|model| model.view.loaded).collect();
    let most_recent = loaded
        .iter()
        .max_by_key(|model| {
            model
                .view
                .residency
                .as_ref()
                .and_then(|residency| residency.last_used_unix_ms)
        })
        .map(|model| model.view.id.clone());
    let count = |filter: &dyn Fn(&Discovered) -> bool| {
        let mut counts = Map::new();
        for kind in MODEL_TYPES {
            let total = models
                .iter()
                .filter(|model| model_type(&model.view.operations) == kind && filter(model))
                .count();
            counts.insert(kind.into(), json!(total));
        }
        Value::Object(counts)
    };
    json!({
        "status": "ok",
        "version": crate::VERSION,
        "telemetry": {"enabled": false},
        "model_loaded": most_recent,
        "all_models_loaded": loaded.iter().map(|model| loaded_entry(model)).collect::<Vec<_>>(),
        "max_models": count(&|model| model.view.policy != Policy::Disabled),
        "pinned_models": count(&|model| model.view.loaded && model.view.policy == Policy::Hot),
        "update_check_done": true,
    })
}

pub async fn catalog(manager: &Arc<Manager>, headers: &HeaderMap) -> Response {
    let models = manager.discovered(true).await;
    let revision = catalog_revision(&models, &aliases(manager, &models));
    let data: Vec<Value> = models.iter().map(Discovered::entry).collect();
    conditional(
        headers,
        &revision,
        "catalog",
        json!({"schema_version": 1, "object": "list", "discovery_schema_version": SCHEMA_VERSION,
            "revision": revision, "data": data}),
    )
}

#[derive(Deserialize)]
pub struct ModelsQuery {
    show_all: Option<String>,
    service: Option<String>,
}

pub async fn models(
    manager: &Arc<Manager>,
    headers: &HeaderMap,
    query: ModelsQuery,
) -> Result<Response, ApiError> {
    let service = query
        .service
        .as_deref()
        .map(|service| match service {
            "chat" => Ok("llm"),
            "transcribe" => Ok("transcription"),
            "speak" => Ok("tts"),
            "images" => Ok("image"),
            "embeddings" => Ok("embedding"),
            _ => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unknown_service_filter",
                "Filter by chat, transcribe, speak, images or embeddings.",
            )),
        })
        .transpose()?;
    let show_all = query.show_all.as_deref() == Some("true");
    let anthropic = headers.contains_key("anthropic-version");
    let models = manager.discovered(true).await;
    let alias_targets = aliases(manager, &models);
    let revision = catalog_revision(&models, &alias_targets);
    let included = |model: &Discovered| {
        (show_all || listed(model))
            && service.is_none_or(|kind| model_type(&model.view.operations) == kind)
            && (!anthropic || model.chat())
    };
    let library = manager.library();
    let mut data: Vec<Value> = models
        .iter()
        .filter(|model| included(model))
        .map(|model| model.openai(&model.view.id, None))
        .collect();
    for entry in &mut data {
        let update = entry["id"]
            .as_str()
            .and_then(|id| library.update_available(id))
            .unwrap_or(false);
        entry["update_available"] = json!(update);
    }
    if service.is_none() && !anthropic {
        for (alias, target) in &alias_targets {
            if let Some(model) = models
                .iter()
                .find(|model| model.view.id == *target && included(model))
            {
                data.push(model.openai(alias, Some(target)));
            }
        }
    }
    let variant = format!(
        "models-u{}{}{}{}",
        library.updates_generation(),
        if show_all { "-all" } else { "" },
        query
            .service
            .as_deref()
            .map(|service| format!("-{service}"))
            .unwrap_or_default(),
        if anthropic { "-anthropic" } else { "" }
    );
    let body = if anthropic {
        let data: Vec<Value> = data
            .iter()
            .map(|model| json!({"id": model["id"], "type": "model", "display_name": model["id"], "created_at": "1970-01-01T00:00:00Z"}))
            .collect();
        json!({"data": data, "has_more": false, "first_id": data.first().map(|model| model["id"].clone()),
            "last_id": data.last().map(|model| model["id"].clone())})
    } else {
        json!({"object": "list", "revision": revision, "discovery_schema_version": SCHEMA_VERSION, "data": data})
    };
    Ok(conditional(headers, &revision, &variant, body))
}

pub async fn capabilities(
    manager: &Arc<Manager>,
    headers: &HeaderMap,
    protocols: Value,
    authenticated: bool,
) -> Response {
    let models = manager.discovered(true).await;
    let revision = catalog_revision(&models, &aliases(manager, &models));
    let contracts: Vec<Value> = models
        .iter()
        .filter(|model| listed(model))
        .filter_map(|model| {
            let mut contract = model.fields.get("capability_contract")?.clone();
            contract["policy"] = json!(model.view.policy);
            contract["thinking"] = model.fields.get("thinking").cloned().unwrap_or(Value::Null);
            contract["tool_calls"] = model
                .fields
                .get("tool_calls")
                .cloned()
                .unwrap_or(Value::Null);
            contract["vision"] = json!(model.view.vision);
            Some(contract)
        })
        .collect();
    conditional(
        headers,
        &revision,
        "capabilities",
        json!({
            "object": "juiced.capabilities", "schema_version": 1,
            "discovery_schema_version": SCHEMA_VERSION,
            "implementation": IMPLEMENTATION,
            "server_version": crate::VERSION, "product_version": crate::VERSION,
            "source_revision": std::env::var("JUICED_SOURCE_REVISION").ok().filter(|value| !value.is_empty()),
            "revision": revision, "models": contracts, "protocols": protocols,
            "scope": {"agent_execution": false, "user_workflows": false, "interaction": "temporary_service_tests"},
            "identity": {"mode": if authenticated { "configured_credentials" } else { "anonymous_trusted_lan" },
                "credential_headers": ["Authorization: Bearer", "x-api-key"],
                "session_ids_are_access_control": false},
        }),
    )
}

pub fn well_known() -> Value {
    json!({
        "name": "Juiced", "version": crate::VERSION, "implementation": IMPLEMENTATION,
        "catalog": "/v1/juiced/catalog", "capabilities": "/v1/juiced/capabilities",
        "capability_schema": 1, "discovery_schema_version": SCHEMA_VERSION,
        "models": "/v1/models", "context": "/v1/juiced/context",
        "protocols": "/v1/juiced/protocols", "inference": "/v1", "ui": "/",
    })
}

/// Read callers see the context budget and engine observation, not process details.
pub fn context(mut value: Value, discovered: Option<Discovered>, operator: bool) -> Value {
    if !operator
        && let Some(applied) = value
            .get_mut("applied")
            .filter(|applied| applied.is_object())
    {
        *applied = json!({"source": applied["source"], "launch": applied["launch"]});
    }
    let context = discovered
        .as_ref()
        .and_then(|model| model.fields.get("context").cloned())
        .unwrap_or(Value::Null);
    value["per_request_tokens"] = context["per_request_tokens"].clone();
    value["per_request_source"] = context["per_request_source"].clone();
    value["discovery"] = context;
    value
}
