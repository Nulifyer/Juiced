use crate::{
    ApiError,
    config::{Operation, Policy, Registration},
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
    routing::{get, post},
};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tower_http::{cors::CorsLayer, services::ServeDir};

pub use crate::auth::Auth;
use crate::auth::{Authorization, RequestContext};

#[derive(Clone)]
struct AppState {
    manager: Arc<Manager>,
    auth: Arc<Authorization>,
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
    let admin = crate::compat::ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.path == raw_path && endpoint.method == method)
        .or_else(|| {
            ENDPOINTS
                .iter()
                .find(|endpoint| endpoint.method == method && route_matches(endpoint.path, path))
        })
        .map_or(
            request.method() != axum::http::Method::GET
                && request.method() != axum::http::Method::HEAD
                || path.starts_with("/juiced/"),
            |endpoint| endpoint.authority == "operator",
        );
    match state.auth.authenticate(request.headers(), admin) {
        Ok(context) => {
            request.extensions_mut().insert(context);
        }
        Err(error) => {
            let mut response = crate::compat::rejection(raw_path, error);
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
            return response;
        }
    }
    let _ingress = if request.method() == axum::http::Method::POST && !admin {
        let maximum = if path == "/audio/transcriptions" {
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
            Err(error) => return crate::compat::rejection(raw_path, error),
        }
    } else {
        None
    };
    let response = next.run(request).await;
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
}
macro_rules! define_routes {
    ($(($method:ident, $path:literal, $handler:ident, $authority:literal)),* $(,)?) => {
        pub const ENDPOINTS: &[Endpoint] = &[$(Endpoint { method: method_name!($method), path: $path, authority: $authority, prefixes: PREFIXES }),*];
        fn native_routes() -> Router<AppState> {
            let router = Router::new();
            $(let router = router.route($path, $method($handler).layer(DefaultBodyLimit::max(if $path == "/audio/transcriptions" { crate::admission::SPEECH_INGRESS_BYTES } else { crate::admission::JSON_INGRESS_BYTES })));)*
            router
        }
    };
}
define_routes! {
    (get, "/health", health, "read"),
    (get, "/models", models, "read"),
    (get, "/system-info", system_info, "operator"),
    (get, "/juiced/catalog", catalog, "read"),
    (get, "/juiced/state", service_state, "read"),
    (get, "/juiced/requests", requests, "operator"),
    (get, "/juiced/protocols", protocols, "read"),
    (get, "/juiced/runtimes", runtime_catalog, "read"),
    (get, "/juiced/runtimes/resolve", runtime_resolve, "read"),
    (get, "/juiced/suitability", suitability, "operator"),
    (get, "/juiced/context", context, "operator"),
    (get, "/juiced/endpoints", endpoints, "read"),
    (get, "/juiced/admission", admission, "operator"),
    (post, "/juiced/admission", pause, "operator"),
    (get, "/models/{model}", model_detail, "read"),
    (get, "/models/{model}/options", model_options, "read"),
    (post, "/models/{model}/options", model_options_set, "operator"),
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
    (post, "/delete", unregister, "operator"),
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
    (get, "/system-stats", system_stats, "operator"),
    (get, "/juiced/logs", logs, "operator"),
    (get, "/juiced/library/search", library_search, "operator"),
    (post, "/juiced/library/pull", library_pull, "operator"),
    (get, "/juiced/library/pulls/{pull}", library_pull_status, "operator"),
}
async fn library_search(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<crate::library::SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.manager.artifacts().ok();
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
async fn health() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        "{\"status\":\"ok\"}",
    )
}
async fn endpoints() -> Json<Value> {
    Json(
        json!({"schema_version":1,"endpoints":ENDPOINTS.iter().chain(crate::compat::ENDPOINTS).collect::<Vec<_>>()}),
    )
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionsChange {
    expected_revision: u64,
    options: crate::profiles::LaunchOptions,
}
async fn model_options_set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(change): ApiJson<OptionsChange>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move {
        state
            .manager
            .set_options(&id, change.expected_revision, change.options)
            .await
            .map(Json)
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "options_interrupted",
            "Profile update was interrupted. Refresh before retrying.",
        )
    })?
}

async fn model_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let model = state
        .manager
        .catalog()
        .await
        .into_iter()
        .find(|model| model.id == id)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_model",
                "The model is not registered.",
            )
        })?;
    Ok(Json(
        json!({"id":model.id,"object":"model","created":0,"owned_by":"juiced","juiced":model}),
    ))
}

async fn register(
    State(state): State<AppState>,
    ApiJson(Registration(spec)): ApiJson<Registration>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move {
        state
            .manager
            .register(spec)
            .await
            .map(|model| Json(json!(model)))
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "registration_interrupted",
            "Registration was interrupted. Inspect the current catalog before retrying.",
        )
    })?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveRegistration {
    model: String,
    expected_revision: u64,
}
async fn unregister(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RemoveRegistration>,
) -> Result<Json<Value>, ApiError> {
    tokio::spawn(async move {
        state
            .manager
            .unregister(&request.model, request.expected_revision)
            .await
            .map(Json)
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "removal_interrupted",
            "Removal was interrupted. Inspect the current catalog before retrying.",
        )
    })?
}

pub fn router(manager: Arc<Manager>, auth: Auth, ui: Option<PathBuf>) -> Router {
    let state = AppState {
        manager,
        auth: Arc::new(Authorization::new(auth)),
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
    let mut app = Router::new();
    for prefix in ["/api/v0", "/api/v1", "/v0", "/v1"] {
        app = app.nest(prefix, routes.clone());
    }
    let mut app = app
        .with_state(state.clone())
        .merge(crate::compat::router(state.manager.clone()))
        .layer(DefaultBodyLimit::max(crate::admission::JSON_INGRESS_BYTES))
        .layer(middleware::from_fn_with_state(state, authorize));
    if let Some(ui) = ui {
        app = app.fallback_service(ServeDir::new(ui).append_index_html_on_directories(true));
    }
    app.layer(
        CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any),
    )
}

async fn catalog(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "schema_version": 1, "data": state.manager.catalog().await }))
}

async fn models(State(state): State<AppState>) -> Json<Value> {
    let models: Vec<_> = state
        .manager
        .catalog()
        .await
        .into_iter()
        .filter(|model| model.policy != Policy::Disabled && model.downloaded)
        .map(|model| json!({"id": model.id, "object": "model", "created": 0, "owned_by": "juiced"}))
        .collect();
    Json(json!({"object": "list", "data": models}))
}

async fn service_state(State(state): State<AppState>) -> Json<Value> {
    Json(
        json!({"schema_version": 1, "version": env!("CARGO_PKG_VERSION"), "implementation": "rust-experiment", "resources": state.manager.resources()}),
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
    state.manager.context(id).await?.into_response().await
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

async fn system_info(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        state.manager.system_info().await?,
    ))
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

async fn protocols() -> Json<Value> {
    Json(json!({"data": [
        {"id": "openai", "support": "partial", "operations": ["models", "chat/completions", "completions", "responses", "embeddings", "audio/transcriptions"], "note": "Forwarded only when the configured worker declares support."},
        {"id": "anthropic", "support": "partial", "note":"Selected text/tool Messages codecs and engine-native token counts; native usage required; advanced hosted tools unsupported."}, {"id": "ollama", "support": "partial", "note":"Chat, raw generate, embeddings and discovery; unsupported options are rejected."},
        {"id": "mcp", "support": "partial", "note":"Stateless JSON Streamable HTTP, explicit-model inference tools and credential/session-scoped cancellation."}, {"id": "automatic1111", "support": "not-implemented"},
        {"id": "realtime", "support": "not-implemented"}, {"id": "wyoming", "support": "not-implemented"}
    ]}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyChange {
    model: String,
    policy: Policy,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAction {
    model: String,
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

async fn load(
    State(state): State<AppState>,
    ApiJson(action): ApiJson<ModelAction>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(state.manager.load(&action.model).await?)))
}

async fn unload(
    State(state): State<AppState>,
    ApiJson(action): ApiJson<ModelAction>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(state.manager.unload(&action.model).await?)))
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

fn request_priority(
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
    lease.speech_dispatched();
    let mut upstream = state
        .manager
        .client
        .post(format!("{base}/inference"))
        .multipart(request.form())
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
async fn forward_prioritized(
    manager: Arc<Manager>,
    body: Bytes,
    operation: Operation,
    priority: crate::admission::Priority,
    cache_context: Option<(&RequestContext, Option<&str>, &str)>,
    automatic: Option<&str>,
) -> Result<Response, ApiError> {
    let original_body = body;
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
    lease.text_dispatched();
    let upstream = manager
        .client
        .post(format!("{base}/v1/{}", operation.path()))
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
