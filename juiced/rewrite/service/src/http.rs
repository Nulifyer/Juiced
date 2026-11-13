use crate::{
    ApiError,
    config::{Operation, Policy},
    extract::{ApiBytes, ApiJson, ApiMultipart, ApiQuery},
    supervisor::{CacheForget, CacheRecovery, CacheRestore, CacheSave, Manager, RequestLease},
};
use axum::{
    Extension, Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tower_http::services::ServeDir;

pub use crate::auth::Auth;
use crate::auth::{Authority, Authorization, RequestContext};
use crate::barista::{
    call as barista_call, settings_set as barista_set, settings_view as barista_view,
};
use crate::connect::{access, health as protocol_health, key_create, key_revoke, protocol_set};
use crate::docs_api::{document as docs_page, index as docs_index};
use crate::generation::{audio_generation, model_3d_generation};
use crate::media::{image_edit, image_upscale, image_variation};
use crate::native_api::{
    native_delete as unregister, native_download_control, native_downloads, native_install,
    native_install_dry_run, native_load as load, native_model_files, native_options_reset,
    native_pull, native_pull_variants, native_register as register, native_registry_search,
    native_system_info as system_info, native_uninstall, native_unload as unload,
};
use crate::storage_api::{storage_reclaim, storage_view};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) manager: Arc<Manager>,
    pub(crate) auth: Arc<Authorization>,
    pub(crate) probe: Arc<std::sync::OnceLock<Router>>,
}

async fn authorize(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let uri = request.uri().clone();
    let raw_path = uri.path();
    let path = native_path(raw_path);
    let method = if request.method() == axum::http::Method::HEAD {
        "GET"
    } else {
        request.method().as_str()
    };
    let authority = crate::compat::ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.path == raw_path && endpoint.method == method)
        .or_else(|| {
            ENDPOINTS
                .iter()
                .find(|endpoint| endpoint.method == method && route_matches(endpoint.path, path))
        })
        .map_or(
            if request.method() != axum::http::Method::GET
                && request.method() != axum::http::Method::HEAD
                || path.starts_with("/juiced/")
                || raw_path.starts_with("/internal/")
            {
                Authority::Operator
            } else {
                Authority::Read
            },
            |endpoint| Authority::from_label(endpoint.authority),
        );
    let admin = authority == Authority::Operator;
    let protocol = crate::protocol_access::Protocol::classify(raw_path);
    let observed = protocol.filter(|_| {
        request
            .extensions()
            .get::<crate::connect::Probe>()
            .is_none()
    });
    let gate = state.manager.protocol_gate().clone();
    let observe = |response: Response| {
        if let Some(protocol) = observed {
            gate.observe(protocol, response.status().as_u16());
        }
        response
    };
    match state.auth.authenticate(request.headers(), authority) {
        Ok(context) => {
            request.extensions_mut().insert(context);
        }
        Err(error) => {
            let mut response = crate::compat::rejection(raw_path, error);
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
            return observe(response);
        }
    }
    if let Some(Err(error)) = protocol.map(|protocol| gate.check(protocol)) {
        return observe(crate::compat::rejection(raw_path, error));
    }
    let _ingress = if request.method() == axum::http::Method::POST && !admin {
        let maximum = if path == "/audio/transcriptions" || crate::media::upload_route(path) {
            crate::admission::SPEECH_INGRESS_BYTES
        } else {
            crate::admission::JSON_INGRESS_BYTES
        };
        let declared_bytes = request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        match state
            .manager
            .reserve_http_ingress(maximum, declared_bytes)
            .await
        {
            Ok(lease) => Some(lease),
            Err(error) => return observe(crate::compat::rejection(raw_path, error)),
        }
    } else {
        None
    };
    let response = observe(next.run(request).await);
    if state.manager.memory_protected() && _ingress.is_some() {
        let (parts, body) = response.into_parts();
        let tracked = stream::unfold(
            (body.into_data_stream(), _ingress),
            |(mut body, claims)| async move { body.next().await.map(|chunk| (chunk, (body, claims))) },
        );
        Response::from_parts(parts, Body::from_stream(tracked))
    } else {
        response
    }
}

fn route_matches(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }
    if !pattern.contains('{') {
        return false;
    }
    let mut actual = path.split('/');
    for expected in pattern.split('/') {
        let Some(segment) = actual.next() else {
            return false;
        };
        if expected.starts_with('{') && expected.ends_with('}') {
            if segment.is_empty() {
                return false;
            }
        } else if segment != expected {
            return false;
        }
    }
    actual.next().is_none()
}

#[derive(serde::Serialize)]
pub struct Endpoint {
    pub method: &'static str,
    pub path: &'static str,
    pub authority: &'static str,
    pub prefixes: &'static [&'static str],
}
const PREFIXES: &[&str] = &["/api/v0", "/api/v1", "/v0", "/v1"];
fn native_path(path: &str) -> &str {
    PREFIXES
        .iter()
        .find_map(|prefix| {
            path.strip_prefix(prefix)
                .filter(|rest| rest.starts_with('/'))
        })
        .unwrap_or(path)
}
macro_rules! method_name {
    (get) => {
        "GET"
    };
    (post) => {
        "POST"
    };
    (delete) => {
        "DELETE"
    };
}
macro_rules! define_routes {
    ($(($method:ident, $path:literal, $handler:ident, $authority:literal)),* $(,)?) => {
        pub const ENDPOINTS: &[Endpoint] = &[$(Endpoint { method: method_name!($method), path: $path, authority: $authority, prefixes: PREFIXES }),*];
        fn native_routes() -> Router<AppState> {
            let router = Router::new();
            $(let router = router.route($path, $method($handler).layer(DefaultBodyLimit::max(if $path == "/juiced/library/upload" { usize::MAX } else if $path == "/audio/transcriptions" || crate::media::upload_route($path) { crate::admission::SPEECH_INGRESS_BYTES } else { crate::admission::JSON_INGRESS_BYTES })));)*
            router
        }
    };
}
define_routes! {
    (get, "/health", health, "read"),
    (get, "/docs", docs_index, "read"),
    (get, "/docs/{*page}", docs_page, "read"),
    (get, "/models", models, "read"),
    (get, "/system-info", system_info, "read"),
    (get, "/juiced/catalog", catalog, "read"),
    (get, "/juiced/state", service_state, "read"),
    (get, "/juiced/requests", requests, "operator"),
    (get, "/juiced/protocols", protocols, "read"),
    (get, "/juiced/runtimes", runtime_catalog, "read"),
    (get, "/juiced/runtimes/resolve", runtime_resolve, "read"),
    (get, "/juiced/suitability", suitability, "operator"),
    (get, "/juiced/context", context, "read"),
    (get, "/juiced/capabilities", capabilities, "read"),
    (get, "/juiced/endpoints", endpoints, "read"),
    (get, "/juiced/admission", admission, "operator"),
    (post, "/juiced/admission", pause, "operator"),
    (get, "/models/{model}", model_detail, "read"),
    (get, "/models/{model}/options", model_options, "read"),
    (post, "/models/{model}/options", model_options_set, "operator"),
    (get, "/models/{model}/variants", model_variants, "operator"),
    (post, "/models/{model}/variants", model_variants_sync, "operator"),
    (get, "/models/{model}/fit", model_fit, "operator"),
    (get, "/models/{model}/runtime", model_runtime, "operator"),
    (post, "/models/{model}/runtime", model_runtime_set, "operator"),
    (get, "/juiced/runtime-packages", runtime_packages, "operator"),
    (post, "/juiced/runtime-packages/import", runtime_import, "operator"),
    (get, "/juiced/engines", engines, "operator"),
    (post, "/juiced/engines/install", engine_install, "operator"),
    (post, "/juiced/models/add", model_add, "operator"),
    (get, "/models/{model}/artifacts", model_artifacts, "operator"),
    (post, "/models/{model}/artifacts", model_artifacts_set, "operator"),
    (post, "/models/register", register, "operator"),
    (post, "/models/check-updates", model_check_updates, "operator"),
    (post, "/delete", unregister, "operator"),
    (post, "/pull", native_pull, "operator"),
    (get, "/pull/variants", native_pull_variants, "operator"),
    (get, "/downloads", native_downloads, "operator"),
    (post, "/downloads/control", native_download_control, "operator"),
    (get, "/registry/search", native_registry_search, "operator"),
    (get, "/models/{model}/files", native_model_files, "read"),
    (delete, "/models/{model}/options", native_options_reset, "operator"),
    (post, "/install", native_install, "operator"),
    (post, "/install/dry-run", native_install_dry_run, "operator"),
    (post, "/uninstall", native_uninstall, "operator"),
    (get, "/juiced/artifacts", artifact_inventory, "operator"),
    (post, "/juiced/artifacts/pull", artifact_pull, "operator"),
    (get, "/juiced/artifacts/jobs/{job}", artifact_job, "operator"),
    (post, "/juiced/artifacts/jobs/{job}", artifact_control, "operator"),
    (post, "/juiced/artifacts/remove", artifact_remove, "operator"),
    (post, "/juiced/artifacts/sets", artifact_set_publish, "operator"),
    (get, "/juiced/artifacts/sets/{set}", artifact_set_get, "operator"),
    (post, "/juiced/artifacts/sets/remove", artifact_set_remove, "operator"),
    (get, "/juiced/cache", cache_view, "operator"),
    (post, "/juiced/cache", cache_configure, "operator"),
    (post, "/juiced/cache/save", cache_save, "operator"),
    (post, "/juiced/cache/restore", cache_restore, "operator"),
    (post, "/juiced/cache/forget", cache_forget, "operator"),
    (post, "/juiced/cache/recover", cache_recover, "operator"),
    (get, "/juiced/cache/inspect", cache_inspect, "operator"),
    (get, "/juiced/cache/sessions", cache_sessions, "operator"),
    (post, "/juiced/cache/sessions", cache_sessions_change, "operator"),
    (post, "/juiced/policy", policy, "operator"),
    (post, "/load", load, "operator"),
    (post, "/unload", unload, "operator"),
    (post, "/chat/completions", chat, "inference"),
    (post, "/completions", completion, "inference"),
    (post, "/responses", responses, "inference"),
    (post, "/embeddings", embeddings, "inference"),
    (post, "/audio/transcriptions", transcription, "inference"),
    (get, "/system-stats", system_stats, "read"),
    (get, "/juiced/logs", logs, "operator"),
    (get, "/juiced/library/search", library_search, "operator"),
    (post, "/juiced/library/pull", library_pull, "operator"),
    (get, "/juiced/library/pulls/{pull}", library_pull_status, "operator"),
    (post, "/juiced/library/pulls/{pull}", library_pull_dismiss, "operator"),
    (get, "/juiced/library/pulls", library_pulls, "operator"),
    (get, "/juiced/library/local", library_local, "operator"),
    (post, "/juiced/library/local/delete", library_local_delete, "operator"),
    (get, "/juiced/storage", storage_view, "read"),
    (post, "/juiced/storage/reclaim", storage_reclaim, "operator"),
    (get, "/juiced/aliases", aliases, "operator"),
    (post, "/juiced/aliases", alias_set, "operator"),
    (post, "/images/generations", image_generation, "inference"),
    (post, "/images/edits", image_edit, "inference"),
    (post, "/images/variations", image_variation, "inference"),
    (post, "/images/upscale", image_upscale, "inference"),
    (post, "/audio/speech", speech, "inference"),
    (post, "/audio/generations", audio_generation, "inference"),
    (post, "/3d/generations", model_3d_generation, "inference"),
    (get, "/juiced/voices", voices, "read"),
    (get, "/juiced/residency", residency, "operator"),
    (post, "/juiced/residency", residency_set, "operator"),
    (post, "/juiced/library/upload", library_upload, "operator"),
    (get, "/juiced/library/import-path", library_import_candidates, "operator"),
    (post, "/juiced/library/import-path", library_import_path, "operator"),
    (post, "/juiced/import/native", native_import, "operator"),
    (get, "/juiced/wyoming", wyoming_view, "operator"),
    (post, "/juiced/wyoming", wyoming_set, "operator"),
    (get, "/realtime", realtime, "inference"),
    (get, "/juiced/flm", flm_status, "operator"),
    (post, "/juiced/flm/pull", flm_pull, "operator"),
    (post, "/juiced/flm/models/add", flm_model_add, "operator"),
    (get, "/juiced/access", access, "operator"),
    (post, "/juiced/access/keys", key_create, "operator"),
    (post, "/juiced/access/keys/revoke", key_revoke, "operator"),
    (post, "/juiced/protocols", protocol_set, "operator"),
    (get, "/juiced/protocols/health", protocol_health, "operator"),
    (get, "/juiced/barista", barista_view, "operator"),
    (post, "/juiced/barista", barista_set, "operator"),
    (post, "/juiced/barista/tools/call", barista_call, "operator"),
    (post, "/juiced/barista/handles/{handle}", barista_handle, "operator"),
    (post, "/classify", classify, "inference"),
    (post, "/rerank", rerank, "inference"),
    (post, "/reranking", rerank, "inference"),
    (post, "/reranker", rerank, "inference"),
    (post, "/tokenize", tokenize, "inference"),
    (post, "/detokenize", detokenize, "inference"),
    (get, "/slots", slots, "inference"),
    (post, "/slots/{slot}", slot_action, "inference"),
    (get, "/stats", stats, "read"),
    (get, "/audio/voices", audio_voices, "read"),
    (get, "/logs/stream", log_stream, "read"),
}
async fn residency(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.residency_view().await),
    )
}
async fn residency_set(
    State(state): State<AppState>,
    ApiJson(change): ApiJson<crate::supervisor::ResidencyChange>,
) -> Result<Json<Value>, ApiError> {
    // A client disconnect must not separate the durable write from its in-memory publication.
    tokio::spawn(async move { state.manager.set_residency(change).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "residency_interrupted",
                "The idle unload change was interrupted. Refresh before retrying.",
            )
        })?
}
async fn flm_status(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.flm_status().await?),
    ))
}
async fn flm_pull(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::FlmPull>,
) -> Result<Response, ApiError> {
    let body = state.manager.flm_pull(request).await?;
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}
async fn flm_model_add(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::FlmModelAdd>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move { state.manager.flm_add_model(request).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_add_interrupted",
                "Automatic registration was interrupted. Inspect the current catalog before retrying.",
            )
        })?
}
async fn aliases(State(state): State<AppState>) -> Json<Value> {
    Json(state.manager.aliases_view().await)
}
async fn alias_set(
    State(state): State<AppState>,
    ApiJson(change): ApiJson<crate::supervisor::AliasChange>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move { state.manager.set_alias(change).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "alias_interrupted",
                "The alias change was interrupted. Refresh before retrying.",
            )
        })?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceQuery {
    model: Option<String>,
}
async fn voices(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<VoiceQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query.model.as_deref().is_some_and(|model| {
        model.is_empty() || model.len() > 256 || model.chars().any(char::is_control)
    }) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_voice_query",
            "Name one bounded registered model ID.",
        ));
    }
    let data = state.manager.voices(query.model.as_deref()).await?;
    let aliases: serde_json::Map<String, Value> = crate::media::OPENAI_VOICE_ALIASES
        .iter()
        .map(|(alias, voice)| ((*alias).to_owned(), json!(voice)))
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"schema_version": 1, "data": data, "aliases": aliases})),
    ))
}

async fn image_generation(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let priority = request_priority(&context, &headers)?;
    if let Some(reply) = crate::generation::thenoise_image(&state.manager, priority, &body).await {
        return reply;
    }
    let request = crate::media::ImageRequest::parse(&body)?;
    let (bytes, mut lease) = media_exchange(
        &state.manager,
        &request.model,
        Operation::ImageGeneration,
        priority,
        "/v1/images/generations",
        request.upstream(),
        crate::media::MAX_IMAGE_RESPONSE_BYTES,
    )
    .await?;
    crate::media::validate_image_response(&bytes, request.format)
        .inspect_err(|_| lease.finish("upstream_error", Some(200)))?;
    lease.finish("completed", Some(200));
    Ok(media_reply("application/json", bytes))
}

async fn speech(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let priority = request_priority(&context, &headers)?;
    if let Some(reply) = crate::generation::openmoss_speech(&state.manager, priority, &body).await {
        return reply;
    }
    let request = crate::media::SpeechRequest::parse(&body)?;
    let (bytes, mut lease) = media_exchange(
        &state.manager,
        &request.model,
        Operation::Speech,
        priority,
        "/v1/audio/speech",
        request.upstream(),
        crate::media::MAX_SPEECH_RESPONSE_BYTES,
    )
    .await?;
    crate::media::validate_speech_response(&bytes, request.format)
        .inspect_err(|_| lease.finish("upstream_error", Some(200)))?;
    lease.finish("completed", Some(200));
    Ok(media_reply(request.format.content_type(), bytes))
}

pub(crate) fn media_reply(content_type: &'static str, bytes: Bytes) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response()
}

/// Single-slot media workers keep their physical claim until the complete reply arrives;
/// dropping the transfer earlier retires that exact worker generation.
pub(crate) async fn media_exchange(
    manager: &Arc<Manager>,
    model: &str,
    operation: Operation,
    priority: crate::admission::Priority,
    path: &'static str,
    body: Vec<u8>,
    limit: usize,
) -> Result<(Bytes, RequestLease), ApiError> {
    media_exchange_with(
        manager,
        model,
        operation,
        priority,
        path,
        limit,
        |request| {
            request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
        },
    )
    .await
}

pub(crate) async fn media_exchange_with(
    manager: &Arc<Manager>,
    model: &str,
    operation: Operation,
    priority: crate::admission::Priority,
    path: &'static str,
    limit: usize,
    body: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<(Bytes, RequestLease), ApiError> {
    let (base, mut lease) = manager.begin_queued(model, operation, priority).await?;
    lease.speech_dispatched();
    let mut upstream = body(manager.client.post(format!("{base}{path}")))
        .timeout(manager.request_timeout)
        .send()
        .await
        .map_err(|error| media_transfer_error(&mut lease, error))?;
    let status = upstream.status();
    let mut output = Vec::with_capacity(
        upstream
            .content_length()
            .unwrap_or(0)
            .min(limit as u64)
            .min(1024 * 1024) as usize,
    );
    while let Some(chunk) = upstream
        .chunk()
        .await
        .map_err(|error| media_transfer_error(&mut lease, error))?
    {
        if output.len().saturating_add(chunk.len()) > limit {
            lease.finish("upstream_error", Some(status.as_u16()));
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "media_response_limit",
                "The media worker response exceeded its bounded result size.",
            ));
        }
        output.extend_from_slice(&chunk);
    }
    lease.speech_reply_eof();
    if !status.is_success() {
        lease.finish("upstream_error", Some(status.as_u16()));
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "media_rejected",
            "The media worker rejected this request. Check its parameters and the selected model.",
        ));
    }
    Ok((Bytes::from(output), lease))
}

fn media_transfer_error(lease: &mut RequestLease, error: reqwest::Error) -> ApiError {
    let timeout = error.is_timeout();
    lease.finish(if timeout { "timeout" } else { "upstream_error" }, None);
    ApiError::new(
        if timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        },
        if timeout {
            "upstream_timeout"
        } else {
            "upstream_unavailable"
        },
        "The media worker transfer failed. Retry or inspect the selected worker.",
    )
}
async fn library_search(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<crate::library::SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.manager.artifacts().ok();
    let _ = state.manager.local_library(false).await;
    let result = state.manager.library().search(store, query).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)))
}
async fn library_pull(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::library::PullRequest>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    state.manager.library().pull(store, request).await.map(Json)
}
async fn library_pull_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let view = state.manager.library().pull_view(&id)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PullAction {
    action: String,
}
async fn library_pull_dismiss(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<PullAction>,
) -> Result<Json<Value>, ApiError> {
    if request.action != "dismiss" {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_library_pull_action",
            "The only library acquisition action is dismiss.",
        ));
    }
    state.manager.library().dismiss_pull(&id).map(Json)
}
async fn library_pulls(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.library().pulls_view()),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalQuery {
    fresh: Option<bool>,
}
async fn library_local(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<LocalQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let inventory = state
        .manager
        .local_library(query.fresh.unwrap_or(false))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(inventory)))
}
async fn library_local_delete(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::LocalDelete>,
) -> Result<Json<Value>, ApiError> {
    // A client disconnect must not interrupt a removal between its check and its effect.
    tokio::spawn(async move { state.manager.delete_local_files(request).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "local_delete_interrupted",
                "Deleting the files was interrupted. Refresh the list before retrying.",
            )
        })?
}
async fn library_upload(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<crate::supervisor::UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    let length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    state
        .manager
        .library_upload(query, length, body)
        .await
        .map(Json)
}
async fn library_import_candidates(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<crate::supervisor::ImportListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let view = state.manager.import_candidates(query).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}
async fn library_import_path(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::ImportPathRequest>,
) -> Result<Json<Value>, ApiError> {
    state.manager.import_path(request).await.map(Json)
}
async fn system_stats(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.system_stats().await),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogQuery {
    after: Option<u64>,
    limit: Option<usize>,
    source: Option<String>,
}
async fn logs(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<LogQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = query.limit.unwrap_or(crate::logs::DEFAULT_PAGE);
    if !(1..=crate::logs::MAXIMUM_PAGE).contains(&limit) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_log_limit",
            format!(
                "Request between 1 and {} log lines.",
                crate::logs::MAXIMUM_PAGE
            ),
        ));
    }
    if query.source.as_deref().is_some_and(|source| {
        source.is_empty() || source.len() > 256 || source.chars().any(char::is_control)
    }) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_log_source",
            "Use service or a bounded model ID as the log source.",
        ));
    }
    let page = state
        .manager
        .logs()
        .read(query.after.unwrap_or(0), limit, query.source.as_deref());
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "schema_version": 1, "data": page.data, "next": page.next,
            "dropped": page.dropped, "oldest": page.oldest, "latest": page.latest,
            "cursor_reset": page.cursor_reset,
            "capacity": {"lines": crate::logs::CAPACITY_LINES, "bytes": crate::logs::CAPACITY_BYTES, "line_bytes": crate::logs::LINE_BYTES},
        })),
    ))
}
async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(crate::discovery_http::health(&state.manager).await)
}
async fn endpoints(State(state): State<AppState>) -> Json<Value> {
    let gate = state.manager.protocol_gate();
    let live = |native: bool| {
        move |endpoint: &&Endpoint| {
            crate::protocol_access::Protocol::of_endpoint(endpoint.path, native)
                .is_none_or(|protocol| gate.enabled(protocol))
        }
    };
    let endpoints: Vec<_> = ENDPOINTS
        .iter()
        .filter(live(true))
        .chain(crate::compat::ENDPOINTS.iter().filter(live(false)))
        .collect();
    Json(json!({"schema_version":1,"endpoints":endpoints}))
}

async fn admission(State(state): State<AppState>) -> Json<Value> {
    Json(state.manager.admission())
}

async fn artifact_task<T: serde::Serialize + Send + 'static>(
    task: impl FnOnce() -> Result<T, crate::artifacts::ArtifactError> + Send + 'static,
) -> Result<Json<Value>, ApiError> {
    let value = tokio::task::spawn_blocking(task).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "artifact_operation_interrupted", "The artifact operation was interrupted. Inspect its current state before retrying."))?.map_err(ApiError::from)?;
    serde_json::to_value(value).map(Json).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "artifact_response",
            "Artifact state could not be represented.",
        )
    })
}
async fn artifact_inventory(
    State(state): State<AppState>,
    ApiQuery(request): ApiQuery<crate::artifacts::InventoryRequest>,
) -> Result<axum::response::Response, ApiError> {
    state.manager.artifact_inventory(request).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactPull {
    expected_generation: u64,
    download: crate::artifacts::DownloadRequest,
}
async fn artifact_pull(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ArtifactPull>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    artifact_task(move || store.start_download(request.download, request.expected_generation)).await
}
async fn artifact_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    artifact_task(move || store.job(&id)).await
}
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactAction {
    Pause,
    Cancel,
    Resume,
    Retry,
    Forget,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactControl {
    expected_revision: u64,
    action: ArtifactAction,
}
async fn artifact_control(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<ArtifactControl>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    artifact_task(move || {
        let job = match request.action {
            ArtifactAction::Pause => store.pause(&id, request.expected_revision)?,
            ArtifactAction::Cancel => store.cancel(&id, request.expected_revision)?,
            ArtifactAction::Resume => store.resume(&id, request.expected_revision)?,
            ArtifactAction::Retry => store.retry(&id, request.expected_revision)?,
            ArtifactAction::Forget => {
                store.forget_job(&id, request.expected_revision)?;
                return Ok(json!({"forgotten":true,"job":id,"artifacts_preserved":true}));
            }
        };
        serde_json::to_value(job).map_err(|_| crate::artifacts::ArtifactError::Corrupt)
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRemove {
    artifact: String,
    expected_generation: u64,
}
async fn artifact_remove(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ArtifactRemove>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    artifact_task(move || {
        store.remove(&request.artifact, request.expected_generation)?;
        Ok(json!({"removed":true,"artifact":request.artifact}))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactSetPublish {
    expected_generation: u64,
    specification: crate::artifacts::ArtifactSetSpec,
}
struct ArtifactSetCancellation(Arc<std::sync::atomic::AtomicBool>);
impl Drop for ArtifactSetCancellation {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}
async fn artifact_set_publish(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ArtifactSetPublish>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _cancellation = ArtifactSetCancellation(cancelled.clone());
    let operation = store.set_operation(cancelled);
    artifact_task(move || {
        store.publish_set(
            request.specification,
            request.expected_generation,
            &operation,
        )
    })
    .await
}
async fn artifact_set_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    artifact_task(move || store.artifact_set(&id)).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactSetRemove {
    set: String,
    expected_generation: u64,
}
async fn artifact_set_remove(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ArtifactSetRemove>,
) -> Result<Json<Value>, ApiError> {
    let store = state.manager.artifacts()?;
    artifact_task(move || {
        store.remove_set(&request.set, request.expected_generation)?;
        Ok(json!({"removed":true,"set":request.set,"artifacts_preserved":true}))
    })
    .await
}

async fn cache_view(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiQuery(page): ApiQuery<crate::cache::PageRequest>,
) -> Result<Response, ApiError> {
    state
        .manager
        .cache_view(&context, page)
        .await?
        .into_response()
        .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheConfiguration {
    enabled: bool,
    expected_generation: u64,
}
async fn cache_configure(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiJson(change): ApiJson<CacheConfiguration>,
) -> Result<Response, ApiError> {
    let reply = tokio::spawn(async move {
        state
            .manager
            .configure_cache(change.enabled, change.expected_generation)
            .await?;
        state.manager.cache_view(&context, Default::default()).await
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cache_configuration_interrupted",
            "Cache configuration was interrupted. Refresh before retrying.",
        )
    })??;
    reply.into_response().await
}
async fn cache_save(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiJson(request): ApiJson<CacheSave>,
) -> Result<Response, ApiError> {
    state
        .manager
        .save_snapshot(request, context)
        .await?
        .into_response()
        .await
}
async fn cache_restore(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiJson(request): ApiJson<CacheRestore>,
) -> Result<Response, ApiError> {
    state
        .manager
        .restore_snapshot(request, context)
        .await?
        .into_response()
        .await
}
async fn cache_forget(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiJson(request): ApiJson<CacheForget>,
) -> Result<Response, ApiError> {
    let reply = tokio::spawn(async move { state.manager.forget_snapshot(request, context).await })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_forget_interrupted",
                "Cache deletion was interrupted. Inspect retained state.",
            )
        })??;
    reply.into_response().await
}
async fn cache_recover(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    ApiJson(request): ApiJson<CacheRecovery>,
) -> Result<Response, ApiError> {
    state
        .manager
        .recover_snapshot(request, context)
        .await?
        .into_response()
        .await
}
async fn cache_sessions(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(state.manager.sessions_view().await?))
}
async fn cache_sessions_change(
    State(state): State<AppState>,
    ApiJson(action): ApiJson<crate::supervisor::SessionAction>,
) -> Result<Json<Value>, ApiError> {
    let manager = state.manager.clone();
    tokio::spawn(async move { manager.session_action(action).await })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_cache_interrupted",
                "The session cache change was interrupted. Refresh before retrying.",
            )
        })?
        .map(Json)
}
async fn cache_inspect(
    State(state): State<AppState>,
    ApiQuery(page): ApiQuery<crate::cache::PageRequest>,
) -> Result<Response, ApiError> {
    state
        .manager
        .inspect_cache(page)
        .await?
        .into_response()
        .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmissionChange {
    paused: bool,
}
async fn pause(
    State(state): State<AppState>,
    ApiJson(change): ApiJson<AdmissionChange>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(state.manager.pause(change.paused)?))
}

async fn model_options(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.manager.options(&id).await.map(Json)
}
async fn model_fit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiQuery(query): ApiQuery<crate::supervisor::FitQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.model_fit(&id, query).await?),
    ))
}
async fn runtime_packages(State(state): State<AppState>) -> Result<Response, ApiError> {
    state
        .manager
        .runtime_packages()
        .await?
        .into_response()
        .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeImportRequest {
    specification_file: PathBuf,
    expected_generation: u64,
}
async fn runtime_import(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RuntimeImportRequest>,
) -> Result<Response, ApiError> {
    state
        .manager
        .import_runtime(request.specification_file, request.expected_generation)
        .await?
        .into_response()
        .await
}
async fn engines(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.engines().await?),
    ))
}
async fn engine_install(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::EngineInstall>,
) -> Result<Response, ApiError> {
    let (status, body) = state.manager.install_engine(request).await?;
    Ok((status, Json(body)).into_response())
}
async fn model_add(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::ModelAdd>,
) -> Result<Json<Value>, crate::supervisor::AddModelError> {
    tokio::spawn(async move { state.manager.add_model(request).await.map(Json) })
        .await
        .map_err(|_| {
            crate::supervisor::AddModelError::from(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_add_interrupted",
                "Automatic registration was interrupted. Inspect the current catalog before retrying.",
            ))
        })?
}
async fn model_runtime(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.manager.runtime_selection(&id).await.map(Json)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeChangeRequest {
    expected_revision: u64,
    change: crate::supervisor::RuntimeChange,
}
async fn model_runtime_set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<RuntimeChangeRequest>,
) -> Result<Json<Value>, ApiError> {
    state
        .manager
        .set_runtime_selection(id, request.expected_revision, request.change)
        .await
        .map(Json)
}
async fn model_artifacts(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.manager.artifact_revision(&id).await.map(Json)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRevisionRequest {
    expected_revision: u64,
    change: crate::supervisor::ArtifactRevisionChange,
}
async fn model_artifacts_set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<ArtifactRevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    state
        .manager
        .set_artifact_revision(id, request.expected_revision, request.change)
        .await
        .map(Json)
}
async fn model_options_set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(change): ApiJson<crate::supervisor::SettingsChange>,
) -> Result<Json<Value>, crate::supervisor::OptionsError> {
    tokio::spawn(async move { state.manager.apply_settings(&id, change).await.map(Json) })
        .await
        .map_err(|_| {
            crate::supervisor::OptionsError::from(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "options_interrupted",
                "Profile update was interrupted. Refresh before retrying.",
            ))
        })?
}

async fn model_variants(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.variants(&id).await?),
    ))
}
async fn model_variants_sync(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<crate::supervisor::VariantSync>,
) -> Result<Json<Value>, ApiError> {
    // A client disconnect must not separate the durable write from its restarts.
    tokio::spawn(async move { state.manager.sync_variants(&id, request).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "variant_sync_interrupted",
                "Copying settings was interrupted. Refresh before retrying.",
            )
        })?
}

async fn model_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let target = state.manager.alias_target(&id);
    let catalog = state.manager.catalog().await;
    let lookup = target
        .as_deref()
        .or_else(|| crate::native_api::namespaced(&id, &catalog))
        .unwrap_or(&id);
    let model = catalog
        .into_iter()
        .find(|model| model.id == lookup)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_model",
                "The model is not registered.",
            )
        })?;
    let mut entry = match state.manager.discovered_model(lookup).await {
        Some(discovered) => discovered.openai(&id, target.as_deref()),
        None => json!({"id":id,"object":"model","created":0,"owned_by":"juiced"}),
    };
    if let Some(target) = target {
        entry["owned_by"] = json!("juiced-alias");
        entry["aliased_to"] = json!(target);
    }
    entry["juiced"] = json!(model);
    Ok(Json(entry))
}

async fn model_check_updates(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    state.manager.check_model_updates().await.map(Json)
}
pub fn router(manager: Arc<Manager>, auth: Auth, ui: Option<PathBuf>) -> Router {
    let wyoming = crate::wyoming::Controller::new(manager.clone());
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let starting = wyoming.clone();
        runtime.spawn(async move { starting.start_configured().await });
    }
    let keys = manager.open_key_store();
    keys.spawn_flusher();
    let state = AppState {
        manager,
        auth: Arc::new(Authorization::with_keys(auth, keys)),
        probe: Arc::default(),
    };
    let routes = native_routes()
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Use a documented method for this endpoint.",
            )
        })
        .fallback(|| async {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_endpoint",
                "This endpoint is not implemented by the Rust experiment.",
            )
        });
    let mut app = Router::new().route("/realtime", get(realtime));
    for prefix in ["/api/v0", "/api/v1", "/v0", "/v1"] {
        app = app.nest(prefix, routes.clone());
    }
    let mut app = app
        .with_state(state.clone())
        .layer(Extension(wyoming))
        .layer(Extension(Arc::new(crate::realtime::Sessions::default())))
        .route(
            "/.well-known/juiced.json",
            get(well_known).with_state(state.clone()),
        )
        .merge(crate::compat::router(state.manager.clone()))
        .merge(crate::a1111::router(state.manager.clone()))
        .merge(crate::metrics::router(
            state.manager.clone(),
            state.auth.clone(),
        ))
        .merge(crate::native_api::internal_router(state.clone()))
        .layer(DefaultBodyLimit::max(crate::admission::JSON_INGRESS_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .merge(crate::llama_routes::public());
    let _ = state.probe.set(app.clone());
    if let Some(ui) = ui {
        app = app.fallback_service(ServeDir::new(ui).append_index_html_on_directories(true));
    }
    app.layer(middleware::from_fn_with_state(
        state.manager.clone(),
        crate::cors::layer,
    ))
}

async fn well_known(State(state): State<AppState>, headers: HeaderMap) -> Response {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::VARY, "authorization, x-api-key"),
        ],
        Json(crate::discovery_http::well_known(
            state.auth.describe(&headers),
        )),
    )
        .into_response()
}

async fn catalog(State(state): State<AppState>, headers: HeaderMap) -> Response {
    crate::discovery_http::catalog(&state.manager, &headers).await
}

async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<crate::discovery_http::ModelsQuery>,
) -> Result<Response, ApiError> {
    crate::discovery_http::models(&state.manager, &headers, query).await
}

async fn capabilities(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let protocols = protocols(State(state.clone())).await.0;
    crate::discovery_http::capabilities(
        &state.manager,
        &headers,
        protocols["data"].clone(),
        state.auth.enforcing(),
    )
    .await
}

async fn service_state(State(state): State<AppState>) -> Json<Value> {
    Json(
        json!({"schema_version": 1, "version": crate::VERSION, "implementation": "rust-experiment", "resources": state.manager.resources(), "residency": state.manager.residency_settings(), "native_adoption": state.manager.native_adoption_status()}),
    )
}

async fn requests(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"data": state.manager.records(), "capacity": 128, "contains_content": false}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuitabilityQuery {
    model: Option<String>,
}

async fn context(
    State(state): State<AppState>,
    Extension(caller): Extension<RequestContext>,
    ApiQuery(query): ApiQuery<SuitabilityQuery>,
) -> Result<Response, ApiError> {
    let id = query
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "model_required",
                "Specify a registered model for context inspection.",
            )
        })?;
    let discovered = state.manager.discovered_model(id).await;
    state
        .manager
        .context(id)
        .await?
        .map(|value| crate::discovery_http::context(value, discovered, caller.operator()))
        .into_response()
        .await
}

async fn suitability(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<SuitabilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = query
        .model
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "model_required",
                "Specify a registered model for suitability inspection.",
            )
        })?;
    Ok(Json(state.manager.suitability(id)?))
}

async fn runtime_catalog(State(state): State<AppState>) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        state.manager.runtime_catalog(),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSelection {
    recipe: String,
    backend: String,
}
async fn runtime_resolve(
    State(state): State<AppState>,
    ApiQuery(selection): ApiQuery<RuntimeSelection>,
) -> Result<Json<crate::catalog::LegacyResolution>, ApiError> {
    if selection.recipe.len() > 256
        || selection.backend.len() > 256
        || selection
            .recipe
            .chars()
            .chain(selection.backend.chars())
            .any(char::is_control)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_runtime_selection",
            "Provide bounded original recipe and backend identifiers.",
        ));
    }
    state
        .manager
        .resolve_runtime(&selection.recipe, &selection.backend)
        .map(Json)
}

async fn protocols(State(state): State<AppState>) -> Json<Value> {
    let gate = state.manager.protocol_gate();
    let mut body = json!({ "data": protocol_inventory() });
    if let Some(data) = body["data"].as_array_mut() {
        data.retain(|entry| {
            entry["id"]
                .as_str()
                .and_then(crate::protocol_access::Protocol::from_id)
                .is_none_or(|protocol| gate.enabled(protocol))
        });
    }
    Json(body)
}

pub(crate) fn protocol_inventory() -> Value {
    json!([
        {"id": "openai", "support": "partial", "operations": ["models", "chat/completions", "completions", "responses", "embeddings", "audio/transcriptions", "audio/speech", "images/generations", "images/edits", "images/variations"], "note": "Forwarded only when the configured worker declares support."},
        {"id": "anthropic", "support": "partial", "note":"Selected text/tool Messages codecs and engine-native token counts; native usage required; advanced hosted tools unsupported."}, {"id": "ollama", "support": "partial", "note":"Chat, raw generate, embeddings and discovery; unsupported options are rejected."},
        {"id": "mcp", "support": "partial", "note":"Stateless JSON Streamable HTTP, explicit-model inference tools and credential/session-scoped cancellation."}, {"id": "automatic1111", "support": "partial", "note":"sd-models, options, samplers, schedulers, progress and txt2img (up to 4 images) through sd.cpp workers; img2img and interrupt return 501."},
        {"id": "realtime", "support": "partial", "note": "WebSocket transcription only: pcm16 at 24 kHz with server VAD."}, {"id": "wyoming", "support": "partial", "note": "Speech to text and text to speech for Home Assistant on its own TCP port. No authentication: listen on a trusted network only."}
    ])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyChange {
    model: String,
    policy: Policy,
    expected_revision: u64,
}

async fn policy(
    State(state): State<AppState>,
    ApiJson(change): ApiJson<PolicyChange>,
) -> Result<Json<Value>, ApiError> {
    // A client disconnect must not separate the durable policy write from its in-memory publication.
    tokio::spawn(async move {
        state
            .manager
            .set_policy(&change.model, change.policy, change.expected_revision)
            .await
            .map(|model| Json(json!(model)))
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "policy_save",
            "The policy operation did not complete.",
        )
    })?
}

async fn chat(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let session = request_session(&headers)?;
    let automatic = automatic_session(&headers)?;
    forward(
        state,
        body,
        Operation::Chat,
        request_priority(&context, &headers)?,
        Some((&context, session, "openai.chat")),
        automatic,
    )
    .await
}
async fn completion(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let session = request_session(&headers)?;
    let automatic = automatic_session(&headers)?;
    forward(
        state,
        body,
        Operation::Completion,
        request_priority(&context, &headers)?,
        Some((&context, session, "openai.completions")),
        automatic,
    )
    .await
}
async fn responses(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let session = request_session(&headers)?;
    let automatic = automatic_session(&headers)?;
    forward(
        state,
        body,
        Operation::Responses,
        request_priority(&context, &headers)?,
        Some((&context, session, "openai.responses")),
        automatic,
    )
    .await
}

async fn embeddings(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let session = request_session(&headers)?;
    let automatic = automatic_session(&headers)?;
    forward(
        state,
        body,
        Operation::Embeddings,
        request_priority(&context, &headers)?,
        Some((&context, session, "openai.embeddings")),
        automatic,
    )
    .await
}

pub(crate) fn request_priority(
    context: &RequestContext,
    headers: &HeaderMap,
) -> Result<crate::admission::Priority, ApiError> {
    use crate::admission::Priority;
    let invalid = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_priority",
            "Send one X-Juiced-Priority value: interactive, batch or essential.",
        )
    };
    if headers.get_all("x-juiced-priority").iter().count() > 1 {
        return Err(invalid());
    }
    match headers
        .get("x-juiced-priority")
        .map(|value| value.to_str().map_err(|_| invalid()))
        .transpose()?
        .unwrap_or("interactive")
    {
        "interactive" => Ok(Priority::Interactive),
        "batch" => Ok(Priority::Batch),
        "essential" if context.operator() => Ok(Priority::Essential),
        "essential" => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "essential_authority",
            "Essential admission requires operator authority.",
        )),
        _ => Err(invalid()),
    }
}

async fn transcription(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiMultipart(multipart): ApiMultipart,
) -> Result<Response, ApiError> {
    let priority = request_priority(&context, &headers)?;
    let request = crate::speech::parse(multipart).await?;
    let (base, mut lease) = state
        .manager
        .begin_queued(&request.model, Operation::Transcription, priority)
        .await?;
    let format = request.response_format;
    let (path, form) = crate::speech::worker_request(lease.adapter(), request)
        .inspect_err(|_| lease.finish("rejected", Some(400)))?;
    lease.speech_dispatched();
    let mut upstream = state
        .manager
        .client
        .post(format!("{base}{path}"))
        .multipart(form)
        .timeout(state.manager.request_timeout)
        .send()
        .await
        .map_err(|error| speech_transfer_error(&mut lease, error))?;
    let status = upstream.status();
    let mut output =
        Vec::with_capacity(upstream.content_length().unwrap_or(0).min(64 * 1024) as usize);
    while let Some(chunk) = upstream
        .chunk()
        .await
        .map_err(|error| speech_transfer_error(&mut lease, error))?
    {
        if output.len().saturating_add(chunk.len()) > crate::speech::MAX_RESPONSE_BYTES {
            lease.finish("upstream_error", Some(status.as_u16()));
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "transcription_response_limit",
                "The speech response exceeded its bounded result size.",
            ));
        }
        output.extend_from_slice(&chunk);
    }
    lease.speech_reply_eof();
    if !status.is_success() {
        lease.finish("upstream_error", Some(status.as_u16()));
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "transcription_rejected",
            "The speech worker rejected this audio request. Check its format and the selected worker.",
        ));
    }
    let validated =
        crate::speech::validate_response(Bytes::from(output), format).inspect_err(|_| {
            lease.finish("upstream_error", Some(status.as_u16()));
        })?;
    lease.finish("completed", Some(status.as_u16()));
    Ok((
        [
            (header::CONTENT_TYPE, validated.content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        validated.bytes,
    )
        .into_response())
}

fn speech_transfer_error(lease: &mut RequestLease, error: reqwest::Error) -> ApiError {
    let timeout = error.is_timeout();
    lease.finish(if timeout { "timeout" } else { "upstream_error" }, None);
    ApiError::new(
        if timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        },
        if timeout {
            "upstream_timeout"
        } else {
            "upstream_unavailable"
        },
        "The speech worker transfer failed. Retry or inspect the selected worker.",
    )
}

pub(crate) fn request_session(headers: &HeaderMap) -> Result<Option<&str>, ApiError> {
    if headers.get_all("x-juiced-session").iter().count() > 1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_session",
            "Provide only one opaque session identifier.",
        ));
    }
    headers
        .get("x-juiced-session")
        .map(|value| {
            value
                .to_str()
                .ok()
                .filter(|value| {
                    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
                })
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_session",
                        "Provide a bounded opaque session identifier.",
                    )
                })
        })
        .transpose()
}

pub(crate) fn automatic_session(headers: &HeaderMap) -> Result<Option<&str>, ApiError> {
    for name in crate::sessions::SESSION_HEADERS {
        let mut values = headers.get_all(name).iter();
        let Some(value) = values.next() else {
            continue;
        };
        if values.next().is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_session",
                "Provide only one opaque session identifier.",
            ));
        }
        return value
            .to_str()
            .ok()
            .filter(|value| {
                !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
            })
            .map(Some)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_session",
                    "Provide a bounded opaque session identifier.",
                )
            });
    }
    Ok(None)
}

pub fn request_model(body: &[u8]) -> Result<std::borrow::Cow<'_, str>, ApiError> {
    #[derive(Deserialize)]
    struct Routing<'a> {
        #[serde(borrow)]
        model: std::borrow::Cow<'a, str>,
    }
    let routing: Routing<'_> = serde_json::from_slice(body).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Provide valid JSON with a registered model ID.",
        )
    })?;
    if routing.model.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "model_required",
            "Provide a registered model ID.",
        ));
    }
    Ok(routing.model)
}

async fn forward(
    state: AppState,
    body: Bytes,
    operation: Operation,
    priority: crate::admission::Priority,
    cache_context: Option<(&RequestContext, Option<&str>, &str)>,
    automatic: Option<&str>,
) -> Result<Response, ApiError> {
    forward_prioritized(
        state.manager,
        body,
        operation,
        priority,
        cache_context,
        automatic,
    )
    .await
}

pub(crate) async fn forward_request(
    manager: Arc<Manager>,
    body: Bytes,
    operation: Operation,
    cache_context: Option<(&RequestContext, Option<&str>, &str)>,
    automatic: Option<&str>,
) -> Result<Response, ApiError> {
    forward_prioritized(
        manager,
        body,
        operation,
        crate::admission::Priority::Interactive,
        cache_context,
        automatic,
    )
    .await
}
pub(crate) async fn forward_prioritized(
    manager: Arc<Manager>,
    body: Bytes,
    operation: Operation,
    priority: crate::admission::Priority,
    cache_context: Option<(&RequestContext, Option<&str>, &str)>,
    automatic: Option<&str>,
) -> Result<Response, ApiError> {
    let original_body = if operation == Operation::Chat && !manager.memory_protected() {
        crate::model_discovery::normalize_thinking(&body).map_or(body, Bytes::from)
    } else {
        body
    };
    let model = request_model(&original_body)?;
    let (base, mut lease, body) = if manager.memory_protected() {
        let prepared = manager.prepare_protected_request(&model, operation, &original_body)?;
        let (base, lease, normalized) = manager.begin_protected_request(prepared).await?;
        (base, lease, Some(normalized))
    } else {
        let (base, lease) = manager.begin_queued(&model, operation, priority).await?;
        (base, lease, None)
    };
    drop(model);
    let mut body = body.unwrap_or(original_body);
    if let Some(routed) = manager
        .prepare_session(
            &mut lease,
            operation,
            &body,
            automatic,
            cache_context.map(|(context, _, _)| context),
        )
        .await?
    {
        body = routed;
    }
    if matches!(
        operation,
        Operation::Chat | Operation::Completion | Operation::Responses
    ) && let Some((context, Some(session), protocol)) = cache_context
    {
        lease.bind_cache(context, session, protocol);
    }
    if let Some(checkpoint) = lease.upstream_model() {
        body = crate::flm::with_model(&body, checkpoint).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "The request body must be a JSON object.",
            )
        })?;
    }
    if lease.adapter() == crate::config::Provider::Vllm
        && let Some(prepared) = crate::vllm::prepare_request(
            &manager.client,
            &base,
            &body,
            matches!(operation, Operation::Chat | Operation::Completion),
            crate::vllm::max_model_len(lease.worker_arguments()),
        )
        .await
    {
        body = Bytes::from(prepared);
    }
    lease.text_dispatched();
    let path = if operation == Operation::Classify {
        "/classify".to_owned()
    } else {
        format!("/v1/{}", operation.path())
    };
    let upstream = manager
        .client
        .post(format!("{base}{path}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .timeout(lease.upstream_timeout())
        .send()
        .await
        .map_err(|error| {
            let timed_out = error.is_timeout();
            lease.finish(
                if timed_out {
                    "timeout"
                } else {
                    "upstream_error"
                },
                None,
            );
            ApiError::new(
                if timed_out {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                },
                if timed_out {
                    "upstream_timeout"
                } else {
                    "upstream_unavailable"
                },
                "The engine request failed. Retry or inspect the selected worker.",
            )
        })?;
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let tail = (status.is_success()
        && matches!(
            operation,
            Operation::Chat | Operation::Completion | Operation::Responses
        ))
    .then(crate::engine_usage::Tail::default);
    let tracked = stream::unfold(
        (upstream.bytes_stream(), lease, false, tail),
        move |(mut upstream, mut lease, failed, mut tail): (
            _,
            RequestLease,
            bool,
            Option<crate::engine_usage::Tail>,
        )| async move {
            if failed {
                return None;
            }
            match upstream.next().await {
                Some(Ok(bytes)) => {
                    if let Some(tail) = tail.as_mut() {
                        tail.push(&bytes);
                    }
                    Some((
                        Ok::<_, std::io::Error>(bytes),
                        (upstream, lease, false, tail),
                    ))
                }
                Some(Err(error)) => {
                    lease.finish(
                        if error.is_timeout() {
                            "timeout"
                        } else {
                            "upstream_error"
                        },
                        Some(status.as_u16()),
                    );
                    Some((
                        Err(std::io::Error::other("The engine response was interrupted")),
                        (upstream, lease, true, None),
                    ))
                }
                None => {
                    lease.text_reply_eof();
                    if let Some(tail) = tail {
                        lease.observe_usage(tail.observe());
                    }
                    lease.finish(
                        if status.is_success() {
                            "completed"
                        } else {
                            "upstream_error"
                        },
                        Some(status.as_u16()),
                    );
                    None
                }
            }
        },
    );
    let mut response = Response::new(Body::from_stream(tracked));
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok(response)
}
async fn native_import(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<crate::supervisor::NativeImportRequest>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move { state.manager.native_import(request).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "native_import_interrupted",
                "The native import was interrupted. Inspect the current catalog before retrying.",
            )
        })?
}
async fn wyoming_view(
    Extension(wyoming): Extension<Arc<crate::wyoming::Controller>>,
) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(wyoming.view().await),
    )
}
async fn wyoming_set(
    Extension(wyoming): Extension<Arc<crate::wyoming::Controller>>,
    ApiJson(change): ApiJson<crate::wyoming::Change>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move { wyoming.configure(change).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wyoming_interrupted",
                "The Wyoming change was interrupted. Refresh before retrying.",
            )
        })?
}
async fn log_stream(
    State(state): State<AppState>,
    upgrade: Result<
        axum::extract::ws::WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Response {
    crate::metrics::log_stream(state.manager, state.auth, upgrade).await
}
async fn realtime(
    State(state): State<AppState>,
    Extension(sessions): Extension<Arc<crate::realtime::Sessions>>,
    ApiQuery(query): ApiQuery<crate::realtime::Query>,
    upgrade: Result<
        axum::extract::ws::WebSocketUpgrade,
        axum::extract::ws::rejection::WebSocketUpgradeRejection,
    >,
) -> Result<Response, ApiError> {
    let upgrade = upgrade.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "websocket_required",
            "Open this endpoint as a WebSocket with ?intent=transcription.",
        )
    })?;
    let slot = sessions.admit(&query)?;
    let manager = state.manager.clone();
    Ok(upgrade
        .protocols(["realtime"])
        .max_message_size(crate::realtime::MAX_MESSAGE_BYTES)
        .max_frame_size(crate::realtime::MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| crate::realtime::serve(manager, socket, query, slot)))
}
use crate::classify::classify;
use crate::llama_routes::{audio_voices, detokenize, rerank, slot_action, slots, stats, tokenize};

use crate::barista::handle as barista_handle;
