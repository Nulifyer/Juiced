//! The v0.9.2 native management API (`docs/api/lemonade.md`) as adapters onto the rewrite's
//! owners: library pulls, artifact jobs, registration, launch options, policy, aliases and
//! engines. Rewrite-shaped bodies on shared paths keep their original behaviour.

use crate::{
    ApiError,
    config::{Policy, Registration},
    engines::catalog::Engine,
    extract::{ApiBytes, ApiQuery},
    http::AppState,
    library::native::{Selection, builtin, parse_checkpoint},
    profiles::LaunchOptions,
    supervisor::{AddModelError, EngineInstall, Manager, ModelAdd, OptionsApply},
};
use axum::{
    Extension, Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(250);
const COMPLETED_RETENTION: Duration = Duration::from_secs(120);
const MAXIMUM_JOBS: usize = 64;
const MAXIMUM_PENDING: usize = 256;
const INSTALL_DEADLINE: Duration = Duration::from_secs(3600);

fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(status, code, message)
}

fn reject(failure: impl IntoResponse) -> Response {
    failure.into_response()
}

fn success(message: String) -> Value {
    json!({"status": "success", "message": message})
}

fn body_value(bytes: &Bytes) -> Result<Value, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(bytes).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Provide valid JSON matching the documented fields and types.",
        )
    })
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|_| {
        error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "Provide valid JSON matching the documented fields and types.",
        )
    })
}

fn interrupted(code: &'static str) -> ApiError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        code,
        "The operation was interrupted. Inspect the current catalog before retrying.",
    )
}

/// The canonical native namespaces; the bare name is the public ID.
fn public_name(name: &str) -> &str {
    ["user.", "extra.", "builtin."]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name)
}

/// The registered public ID behind a native `user.X`-style name, when only that one exists.
pub(crate) fn namespaced<'a>(
    name: &'a str,
    catalog: &[crate::supervisor::ModelView],
) -> Option<&'a str> {
    let public = public_name(name);
    (public != name
        && !catalog.iter().any(|model| model.id == name)
        && catalog.iter().any(|model| model.id == public))
    .then_some(public)
}

async fn registered(manager: &Manager, name: &str) -> Option<String> {
    let catalog = manager.catalog().await;
    let known = |id: &str| catalog.iter().any(|model| model.id == id);
    if known(name) {
        return Some(name.to_owned());
    }
    if let Some(target) = manager.alias_target(name).filter(|target| known(target)) {
        return Some(target);
    }
    let public = public_name(name);
    known(public).then(|| public.to_owned())
}

fn not_found(name: &str) -> ApiError {
    error(
        StatusCode::NOT_FOUND,
        "model_not_found",
        format!("Model not found: {name}"),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAction {
    model: String,
}

// ---------------------------------------------------------------------------------------------
// Pending registrations and download jobs.

/// A `POST /models/register` definition waiting for `POST /pull`. The rewrite registers a
/// model only once its weights exist, so this lives in memory until the pull completes.
#[derive(Clone, Debug)]
struct Pending {
    recipe: String,
    checkpoint: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Progress {
    status: &'static str,
    running: bool,
    file: String,
    file_index: usize,
    total_files: usize,
    bytes_downloaded: u64,
    bytes_total: u64,
    total_download_size: u64,
    bytes_previously_downloaded: u64,
    completed_files_bytes: u64,
    cumulative_bytes_downloaded: u64,
    percent: u64,
    complete: bool,
    error: Option<String>,
    code: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    None,
    Pause,
    Cancel,
}

struct Download {
    id: String,
    model_name: String,
    progress: Mutex<Progress>,
    stop: Mutex<Stop>,
    pull: Mutex<Option<String>>,
    finished: Mutex<Option<Instant>>,
}

impl Download {
    fn snapshot(&self) -> Progress {
        self.progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    fn update(&self, change: impl FnOnce(&mut Progress)) {
        change(
            &mut self
                .progress
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
    }
    fn finish(&self, change: impl FnOnce(&mut Progress)) {
        self.update(|progress| {
            change(progress);
            progress.running = false;
        });
        *self
            .finished
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
    }
    fn view(&self) -> Value {
        let progress = self.snapshot();
        let mut view = json!({
            "id": self.id, "type": "model", "model_name": self.model_name,
            "status": progress.status, "running": progress.running,
            "file": progress.file, "file_index": progress.file_index,
            "total_files": progress.total_files, "bytes_downloaded": progress.bytes_downloaded,
            "bytes_total": progress.bytes_total, "total_download_size": progress.total_download_size,
            "bytes_previously_downloaded": progress.bytes_previously_downloaded,
            "completed_files_bytes": progress.completed_files_bytes,
            "cumulative_bytes_downloaded": progress.cumulative_bytes_downloaded,
            "overall_bytes_downloaded": progress.cumulative_bytes_downloaded,
            "percent": progress.percent, "complete": progress.complete,
        });
        if let Some(message) = progress.error {
            view["error"] = json!(message);
        }
        view
    }
    fn terminal(&self) -> bool {
        !self.snapshot().running
    }
}

#[derive(Default)]
struct Registry {
    jobs: Mutex<Vec<Arc<Download>>>,
    pending: Mutex<BTreeMap<String, Pending>>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

impl Registry {
    fn prune(&self) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
        jobs.retain(|job| {
            let progress = job.snapshot();
            !(progress.complete
                && job
                    .finished
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .is_some_and(|finished| finished.elapsed() > COMPLETED_RETENTION))
        });
    }
    fn find(&self, id: &str) -> Option<Arc<Download>> {
        self.jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|job| job.id == id)
            .cloned()
    }
    fn remove(&self, id: &str) {
        self.jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|job| job.id != id);
    }
    fn remember(&self, public: &str, pending: Pending) -> Result<(), ApiError> {
        let mut entries = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.len() >= MAXIMUM_PENDING && !entries.contains_key(public) {
            return Err(error(
                StatusCode::CONFLICT,
                "registration_limit",
                format!(
                    "At most {MAXIMUM_PENDING} models can wait for a pull. Pull or delete some first."
                ),
            ));
        }
        entries.insert(public.to_owned(), pending);
        Ok(())
    }
    fn pending(&self, public: &str) -> Option<Pending> {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(public)
            .cloned()
    }
}

// ---------------------------------------------------------------------------------------------
// Pull planning.

#[derive(Default, Deserialize)]
struct PullBody {
    model_name: String,
    #[serde(default)]
    checkpoint: Option<String>,
    #[serde(default)]
    checkpoints: Option<BTreeMap<String, String>>,
    #[serde(default)]
    recipe: Option<String>,
    #[serde(default)]
    mmproj: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    components: Option<Vec<String>>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    subscribe: Option<bool>,
}

enum Plan {
    Installed(String),
    Download {
        public: String,
        selection: Selection,
    },
}

fn unknown_model(name: &str) -> ApiError {
    error(
        StatusCode::BAD_REQUEST,
        "unknown_model",
        format!(
            "When registering a new model, the model name must include the `user` namespace, for example `user.Phi-4-Mini-GGUF`. Received: {name}"
        ),
    )
}

fn recipe_engine(recipe: &str) -> Result<Engine, ApiError> {
    Engine::parse(recipe).ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "unsupported_recipe",
            format!("Recipe '{recipe}' has no Juiced engine."),
        )
    })
}

fn registry_source(source: Option<&str>) -> Result<(), ApiError> {
    match source {
        None | Some("huggingface" | "hf") => Ok(()),
        Some("modelscope" | "ms") => Err(error(
            StatusCode::BAD_REQUEST,
            "unsupported_source",
            "Juiced pulls from Hugging Face only; ModelScope is not available.",
        )),
        Some(_) => Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_source",
            "Use source huggingface or modelscope.",
        )),
    }
}

async fn plan(manager: &Manager, body: &PullBody) -> Result<Plan, ApiError> {
    let name = body.model_name.trim();
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_model_name",
            "Provide a model_name of at most 256 characters.",
        ));
    }
    if body
        .components
        .as_ref()
        .is_some_and(|components| !components.is_empty())
        || body
            .recipe
            .as_deref()
            .is_some_and(|recipe| recipe.starts_with("collection."))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "collection_unsupported",
            "Collections are not available in Juiced. Pull each component model separately.",
        ));
    }
    registry_source(body.source.as_deref())?;
    if let Some(id) = registered(manager, name).await {
        return Ok(Plan::Installed(id));
    }
    let public = public_name(name).to_owned();
    let pending = registry().pending(&public);
    let checkpoint = body
        .checkpoint
        .clone()
        .or_else(|| {
            body.checkpoints
                .as_ref()
                .and_then(|checkpoints| checkpoints.get("main").cloned())
        })
        .or_else(|| {
            pending
                .as_ref()
                .and_then(|pending| pending.checkpoint.clone())
        });
    let library = manager.library();
    let selection = if let Some(checkpoint) = checkpoint {
        if !name.starts_with("user.") && pending.is_none() {
            return Err(unknown_model(name));
        }
        let recipe = body
            .recipe
            .clone()
            .or_else(|| pending.as_ref().map(|pending| pending.recipe.clone()))
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "recipe_required",
                    "Registering a model requires a recipe, for example llamacpp.",
                )
            })?;
        recipe_engine(&recipe)?;
        let (repo, variant) = parse_checkpoint(&checkpoint).ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "invalid_checkpoint",
                "Use a Hugging Face checkpoint like owner/repo or owner/repo:Q4_K_M.",
            )
        })?;
        let selection = library
            .select_checkpoint(&repo, variant.as_deref(), None)
            .await?;
        registry().remember(
            &public,
            Pending {
                recipe,
                checkpoint: Some(checkpoint.clone()),
            },
        )?;
        let extras = body
            .mmproj
            .iter()
            .cloned()
            .chain(body.checkpoints.iter().flat_map(|checkpoints| {
                checkpoints
                    .iter()
                    .filter(|(role, _)| role.as_str() != "main")
                    .map(|(_, value)| value.clone())
            }))
            .collect::<Vec<_>>();
        if let Some(extra) = extras.iter().find(|extra| {
            let file = extra.rsplit(':').next().unwrap_or(extra);
            !selection
                .files
                .iter()
                .any(|known| known == file || known.ends_with(&format!("/{file}")))
        }) {
            return Err(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "companion_unsupported",
                format!(
                    "Juiced cannot pull '{extra}' beside this checkpoint yet. Pull the main file, or choose a catalog model that bundles its projector."
                ),
            ));
        }
        selection
    } else if let Some(entry) = builtin(name) {
        library
            .select_checkpoint(
                &entry.repo,
                None,
                entry.file.as_deref().or(entry.quant.as_deref()),
            )
            .await?
    } else {
        return Err(unknown_model(name));
    };
    Ok(Plan::Download { public, selection })
}

// ---------------------------------------------------------------------------------------------
// Download driver.

fn observe(view: &Value, progress: &mut Progress) {
    let files = view["files"].as_array().cloned().unwrap_or_default();
    let present = |file: &Value| file["state"] == "present";
    let bytes = |file: &Value| file["bytes"].as_u64().unwrap_or(0);
    progress.total_files = files.len();
    progress.total_download_size = files.iter().map(bytes).sum();
    let current = files
        .iter()
        .position(|file| !present(file))
        .unwrap_or(files.len().saturating_sub(1));
    progress.completed_files_bytes = files
        .iter()
        .take(current)
        .filter(|file| present(file))
        .map(bytes)
        .sum();
    if let Some(file) = files.get(current) {
        progress.file = file["file"]
            .as_str()
            .map(|path| path.rsplit('/').next().unwrap_or(path).to_owned())
            .unwrap_or_default();
        progress.file_index = current + 1;
        progress.bytes_total = bytes(file);
        progress.bytes_downloaded = if present(file) {
            bytes(file)
        } else {
            file["downloaded_bytes"].as_u64().unwrap_or(0)
        };
        progress.bytes_previously_downloaded = if present(file) { bytes(file) } else { 0 };
    }
    progress.cumulative_bytes_downloaded =
        progress.completed_files_bytes + progress.bytes_downloaded;
    progress.percent = progress
        .bytes_downloaded
        .saturating_mul(100)
        .checked_div(progress.bytes_total)
        .unwrap_or(0)
        .min(100);
}

async fn control_jobs(manager: &Manager, pull: &str, stop: Stop) {
    let Ok(store) = manager.artifacts() else {
        return;
    };
    let Ok(view) = manager.library().pull_view(pull) else {
        return;
    };
    let jobs: Vec<String> = view["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter_map(|job| job.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let _ = tokio::task::spawn_blocking(move || {
        for job in jobs {
            let Ok(current) = store.job(&job) else {
                continue;
            };
            let _ = match stop {
                Stop::Pause => store.pause(&job, current.revision).map(drop),
                Stop::Cancel => store.cancel(&job, current.revision).map(drop),
                Stop::None => store.resume(&job, current.revision).map(drop),
            };
        }
    })
    .await;
}

async fn start_pull(manager: &Manager, selection: &Selection) -> Result<String, ApiError> {
    let store = manager.artifacts()?;
    let mut attempt = 0;
    loop {
        let request = crate::library::PullRequest {
            repo: selection.repo.clone(),
            file: (selection.files.len() == 1).then(|| selection.files[0].clone()),
            files: (selection.files.len() > 1).then(|| selection.files.clone()),
            revision: None,
            expected_generation: store.generation(),
        };
        match manager.library().pull(store.clone(), request).await {
            Ok(view) => {
                return view["pull"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| interrupted("library_pull_identifier"));
            }
            Err(failure) if failure.status == StatusCode::CONFLICT && attempt < 3 => {
                attempt += 1;
                tokio::time::sleep(POLL).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

fn add_failure(failure: AddModelError) -> (String, String) {
    match failure {
        AddModelError::Api(failure) => (failure.code.to_owned(), failure.message.clone()),
        AddModelError::NotInstalled(body) => {
            let install = &body["install"];
            let engine = install["engine"].as_str().unwrap_or("llama.cpp");
            let backend = install["backend"].as_str().unwrap_or("cpu");
            let recipe = Engine::parse(engine).map_or(engine, |engine| engine.recipe());
            (
                "backend_not_installed".into(),
                format!(
                    "The weights are downloaded, but no engine is installed for them. Run `lemonade backends install {recipe}:{backend}`, then pull again."
                ),
            )
        }
    }
}

/// Registers downloaded weights on the recommended backend, or else on any installed one: a
/// native pull never installs engines, so an installed CPU build must not lose to an
/// uninstalled recommendation.
async fn register_download(
    manager: &Arc<Manager>,
    request: ModelAdd,
) -> Result<Value, AddModelError> {
    let retry = |backend: Option<String>| ModelAdd {
        artifact: request.artifact.clone(),
        artifact_set: request.artifact_set.clone(),
        id: request.id.clone(),
        policy: request.policy,
        backend,
        path: None,
    };
    let failure = match manager.add_model(retry(None)).await {
        Err(AddModelError::NotInstalled(body)) => body,
        other => return other,
    };
    let engine = failure["install"]["engine"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let installed: Vec<String> = manager
        .engines()
        .await
        .ok()
        .and_then(|engines| engines["installable"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            row["engine"] == engine.as_str()
                && row["installed_package"].is_string()
                && row["availability"] != "unavailable"
        })
        .filter_map(|row| row["backend"].as_str().map(str::to_owned))
        .collect();
    for backend in installed {
        match manager.add_model(retry(Some(backend))).await {
            Err(AddModelError::NotInstalled(_) | AddModelError::Api(_)) => continue,
            other => return other,
        }
    }
    Err(AddModelError::NotInstalled(failure))
}

async fn drive(manager: Arc<Manager>, job: Arc<Download>, public: String, selection: Selection) {
    let pull = match start_pull(&manager, &selection).await {
        Ok(pull) => pull,
        Err(failure) => {
            job.finish(|progress| {
                progress.status = "error";
                progress.error = Some(failure.message.clone());
                progress.code = Some(failure.code.to_owned());
            });
            return;
        }
    };
    *job.pull.lock().unwrap_or_else(|error| error.into_inner()) = Some(pull.clone());
    control_jobs(&manager, &pull, Stop::None).await;
    let view = loop {
        let view = match manager.library().pull_view(&pull) {
            Ok(view) => view,
            Err(failure) => {
                job.finish(|progress| {
                    progress.status = "error";
                    progress.error = Some(failure.message.clone());
                    progress.code = Some(failure.code.to_owned());
                });
                return;
            }
        };
        job.update(|progress| observe(&view, progress));
        let stop = *job.stop.lock().unwrap_or_else(|error| error.into_inner());
        if stop != Stop::None {
            control_jobs(&manager, &pull, stop).await;
            job.finish(|progress| {
                progress.status = if stop == Stop::Pause {
                    "paused"
                } else {
                    "cancelled"
                };
            });
            return;
        }
        match view["state"].as_str() {
            Some("complete") => break view,
            Some("failed") => {
                let code = view["error"].as_str().unwrap_or("library_pull_failed");
                job.finish(|progress| {
                    progress.status = "error";
                    progress.error = Some(format!("The download failed ({code})."));
                    progress.code = Some(code.to_owned());
                });
                let _ = manager.library().dismiss_pull(&pull);
                return;
            }
            _ => tokio::time::sleep(POLL).await,
        }
    };
    let set = view["set"].as_str().map(str::to_owned);
    let artifact = set.is_none().then(|| {
        view["files"][0]["sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    });
    let request = ModelAdd {
        artifact,
        artifact_set: set,
        id: Some(public.clone()),
        policy: Some(Policy::OnDemand),
        backend: None,
        path: None,
    };
    let added = register_download(&manager, request).await;
    let _ = manager.library().dismiss_pull(&pull);
    match added {
        Ok(_) => {
            registry()
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&public);
            job.finish(|progress| {
                progress.status = "completed";
                progress.complete = true;
                progress.percent = 100;
                progress.bytes_downloaded = progress.bytes_total;
                progress.cumulative_bytes_downloaded = progress.total_download_size;
            });
        }
        Err(failure) => {
            let (code, message) = add_failure(failure);
            job.finish(|progress| {
                progress.status = "error";
                progress.error = Some(message);
                progress.code = Some(code);
            });
        }
    }
}

fn begin(
    manager: &Arc<Manager>,
    name: &str,
    public: String,
    selection: Selection,
) -> Arc<Download> {
    let registry = registry();
    registry.prune();
    let id = format!("model:{name}");
    let mut jobs = registry
        .jobs
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(existing) = jobs.iter().find(|job| job.id == id)
        && !existing.terminal()
    {
        return existing.clone();
    }
    jobs.retain(|job| job.id != id);
    while jobs.len() >= MAXIMUM_JOBS {
        if let Some(index) = jobs.iter().position(|job| job.terminal()) {
            jobs.remove(index);
        } else {
            break;
        }
    }
    let job = Arc::new(Download {
        id,
        model_name: name.to_owned(),
        progress: Mutex::new(Progress {
            status: "downloading",
            running: true,
            ..Progress::default()
        }),
        stop: Mutex::new(Stop::None),
        pull: Mutex::new(None),
        finished: Mutex::new(None),
    });
    jobs.push(job.clone());
    tokio::spawn(drive(manager.clone(), job.clone(), public, selection));
    job
}

fn event(kind: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {kind}\ndata: {data}\n\n"))
}

fn progress_event(progress: &Progress) -> Value {
    json!({
        "file": progress.file, "file_index": progress.file_index,
        "total_files": progress.total_files, "bytes_downloaded": progress.bytes_downloaded,
        "bytes_total": progress.bytes_total, "bytes_previously_downloaded": progress.bytes_previously_downloaded,
        "total_download_size": progress.total_download_size, "percent": progress.percent,
    })
}

fn terminal_event(progress: &Progress) -> Bytes {
    if progress.complete {
        event(
            "complete",
            json!({"file_index": progress.total_files, "total_files": progress.total_files, "percent": 100}),
        )
    } else {
        let message = progress
            .error
            .clone()
            .unwrap_or_else(|| format!("The download stopped ({}).", progress.status));
        event(
            "error",
            json!({"error": message, "code": progress.code.clone().unwrap_or_else(|| progress.status.to_owned())}),
        )
    }
}

fn sse(stream: impl futures_util::Stream<Item = Bytes> + Send + 'static) -> Response {
    use futures_util::StreamExt;
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream.map(Ok::<_, std::convert::Infallible>)),
    )
        .into_response()
}

fn job_stream(job: Arc<Download>) -> Response {
    let stream = futures_util::stream::unfold(
        (job, None::<String>, false),
        |(job, last, done)| async move {
            if done {
                return None;
            }
            loop {
                let progress = job.snapshot();
                if !progress.running {
                    return Some((terminal_event(&progress), (job, last, true)));
                }
                let data = progress_event(&progress);
                let text = data.to_string();
                if progress.total_files > 0 && last.as_deref() != Some(text.as_str()) {
                    return Some((event("progress", data), (job, Some(text), false)));
                }
                tokio::time::sleep(POLL).await;
            }
        },
    );
    sse(stream)
}

async fn settle(job: &Download) -> Progress {
    loop {
        let progress = job.snapshot();
        if !progress.running {
            return progress;
        }
        tokio::time::sleep(POLL).await;
    }
}

fn outcome(name: &str, progress: Progress) -> Result<Json<Value>, ApiError> {
    if progress.complete {
        return Ok(Json(success(format!("Installed model: {name}"))));
    }
    Err(ApiError::new(
        if progress.code.as_deref() == Some("backend_not_installed") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_GATEWAY
        },
        "model_pull_failed",
        progress
            .error
            .unwrap_or_else(|| format!("The download stopped ({}).", progress.status)),
    ))
}

pub(crate) async fn native_pull(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let body: PullBody = decode(body_value(&bytes)?)?;
    let name = body.model_name.trim().to_owned();
    let planned = plan(&state.manager, &body).await;
    if body.stream && body.subscribe != Some(false) {
        return Ok(match planned {
            Ok(Plan::Installed(_)) => sse(futures_util::stream::iter([event(
                "complete",
                json!({"file_index": 0, "total_files": 0, "percent": 100}),
            )])),
            Ok(Plan::Download { public, selection }) => {
                job_stream(begin(&state.manager, &name, public, selection))
            }
            Err(failure) => sse(futures_util::stream::iter([event(
                "error",
                json!({"error": failure.message, "code": failure.code}),
            )])),
        });
    }
    match planned? {
        Plan::Installed(_) if body.stream => Ok(Json(json!({
            "id": format!("model:{name}"), "type": "model", "model_name": name,
            "status": "completed", "running": false, "percent": 100, "complete": true,
        }))
        .into_response()),
        Plan::Installed(_) => Ok(Json(success(format!("Installed model: {name}"))).into_response()),
        Plan::Download { public, selection } => {
            let job = begin(&state.manager, &name, public, selection);
            if body.stream {
                return Ok(Json(job.view()).into_response());
            }
            outcome(&name, settle(&job).await).map(IntoResponse::into_response)
        }
    }
}

pub(crate) async fn native_downloads() -> Json<Value> {
    let registry = registry();
    registry.prune();
    let jobs = registry
        .jobs
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    Json(Value::Array(jobs.iter().map(|job| job.view()).collect()))
}

#[derive(Deserialize)]
struct DownloadControl {
    id: String,
    action: String,
}

pub(crate) async fn native_download_control(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let request: DownloadControl = decode(body_value(&bytes)?)?;
    let registry = registry();
    let job = registry.find(&request.id);
    let stop = match request.action.as_str() {
        "pause" => Stop::Pause,
        "cancel" | "remove" => Stop::Cancel,
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "invalid_download_action",
                "Use action pause, cancel or remove.",
            ));
        }
    };
    let Some(job) = job else {
        if request.action == "remove" {
            return Ok(Json(json!({"status": "ok", "missing": true})));
        }
        return Err(error(
            StatusCode::NOT_FOUND,
            "download_not_found",
            "Download not found",
        ));
    };
    if job.terminal() {
        if request.action == "remove" {
            registry.remove(&job.id);
            let pull = job
                .pull
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(pull) = pull {
                if job.snapshot().status == "cancelled" {
                    control_jobs(&state.manager, &pull, Stop::Cancel).await;
                }
                let _ = state.manager.library().dismiss_pull(&pull);
            }
            return Ok(Json(json!({"status": "ok"})));
        }
        if stop == Stop::Cancel && job.snapshot().status == "paused" {
            let pull = job
                .pull
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(pull) = pull {
                control_jobs(&state.manager, &pull, Stop::Cancel).await;
            }
            job.update(|progress| progress.status = "cancelled");
        }
        return Ok(Json(job.view()));
    }
    *job.stop.lock().unwrap_or_else(|error| error.into_inner()) = stop;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !job.terminal() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(Json(job.view()))
}

// ---------------------------------------------------------------------------------------------
// Search and variants.

#[derive(Deserialize)]
pub(crate) struct RegistrySearch {
    query: Option<String>,
    q: Option<String>,
    source: Option<String>,
    limit: Option<String>,
    format: Option<String>,
}

pub(crate) async fn native_registry_search(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<RegistrySearch>,
) -> Result<Json<Value>, ApiError> {
    let text = query
        .query
        .or(query.q)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if text.chars().count() < 3 {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "Query parameter 'query' must contain at least 3 characters",
        ));
    }
    registry_source(query.source.as_deref())?;
    let limit = match query.limit.as_deref() {
        None => 12,
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|limit| (1..=50).contains(limit))
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "invalid_limit",
                    "Query parameter 'limit' must be an integer from 1 to 50",
                )
            })?,
    };
    if query
        .format
        .as_deref()
        .is_some_and(|format| format != "gguf")
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_format",
            "The only accepted format is gguf",
        ));
    }
    let store = state.manager.artifacts().ok();
    let result = state
        .manager
        .library()
        .search(
            store,
            crate::library::SearchQuery {
                q: Some(text.clone()),
                hub: Some(true),
                task: None,
                limit: Some(limit),
                cursor: None,
            },
        )
        .await?;
    if result["offline"] == true {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "lemond_offline",
            "The server is offline; remote registry search is unavailable.",
        ));
    }
    let results: Vec<Value> = result["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["source"] == "huggingface")
        .take(limit)
        .map(|entry| {
            let gguf = entry["quants"]
                .as_array()
                .is_some_and(|quants| !quants.is_empty());
            json!({
                "repository_id": entry["repo"], "display_name": entry["name"],
                "source": "huggingface", "repository_type": "model",
                "description": "", "tags": if gguf { json!(["gguf"]) } else { json!([]) },
                "task": entry["pipeline_tag"], "downloads": entry["downloads"].as_u64().unwrap_or(0),
                "likes": entry["likes"].as_u64().unwrap_or(0), "has_gguf": gguf,
            })
        })
        .collect();
    let mut body =
        json!({"source": "huggingface", "query": text, "total": results.len(), "results": results});
    if query.format.is_some() {
        body["format"] = json!("gguf");
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
pub(crate) struct VariantsQuery {
    checkpoint: Option<String>,
}

pub(crate) async fn native_pull_variants(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<VariantsQuery>,
) -> Result<Json<Value>, ApiError> {
    let checkpoint = query.checkpoint.unwrap_or_default();
    if checkpoint.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "checkpoint_required",
            "Missing required query parameter 'checkpoint'",
        ));
    }
    let (repo, _) = parse_checkpoint(&checkpoint).ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_checkpoint",
            "The checkpoint must be a Hugging Face repository like owner/repo.",
        )
    })?;
    state
        .manager
        .library()
        .native_variants(&repo)
        .await
        .map(Json)
}

// ---------------------------------------------------------------------------------------------
// Registration, deletion, load and unload.

pub(crate) async fn native_register(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let body = body_value(&bytes)?;
    if body.get("model_name").is_none() {
        let Registration(spec) = decode(body)?;
        let manager = state.manager.clone();
        return tokio::spawn(async move {
            manager.register(spec).await.map(|model| Json(json!(model)))
        })
        .await
        .map_err(|_| interrupted("registration_interrupted"))?;
    }
    let body: PullBody = decode(body)?;
    let name = body.model_name.trim();
    let public = public_name(name);
    if !name.starts_with("user.") || public == name {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_model_name",
            format!(
                "Registered model definitions must use a non-empty `user.*` name, for example `user.Phi-4-Mini-GGUF`. Received: {name}"
            ),
        ));
    }
    let recipe = body.recipe.clone().ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "recipe_required",
            "Registering a model requires a recipe, for example llamacpp.",
        )
    })?;
    recipe_engine(&recipe)?;
    registry_source(body.source.as_deref())?;
    let checkpoint = body.checkpoint.clone().or_else(|| {
        body.checkpoints
            .as_ref()
            .and_then(|checkpoints| checkpoints.get("main").cloned())
    });
    if checkpoint
        .as_deref()
        .is_some_and(|checkpoint| parse_checkpoint(checkpoint).is_none())
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_checkpoint",
            "Use a Hugging Face checkpoint like owner/repo or owner/repo:Q4_K_M.",
        ));
    }
    let installed = registered(&state.manager, public).await.is_some();
    let labels = body.labels.clone().unwrap_or_default();
    if !installed {
        registry().remember(
            public,
            Pending {
                recipe: recipe.clone(),
                checkpoint: checkpoint.clone(),
            },
        )?;
    }
    Ok(Json(json!({
        "status": "success", "model_name": public, "canonical_model_name": name,
        "model": {
            "id": public, "recipe": recipe, "checkpoint": checkpoint,
            "checkpoints": checkpoint.as_ref().map(|main| json!({"main": main})).unwrap_or_else(|| json!({})),
            "labels": labels, "downloaded": installed, "source": "huggingface",
            "registration": if installed { "installed" } else { "pending_pull" },
        },
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveRegistration {
    model: String,
    expected_revision: u64,
}

pub(crate) async fn native_delete(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let body = body_value(&bytes)?;
    let manager = state.manager.clone();
    if body.get("expected_revision").is_some() {
        let request: RemoveRegistration = decode(body)?;
        return tokio::spawn(async move {
            manager
                .unregister(&request.model, request.expected_revision)
                .await
                .map(Json)
        })
        .await
        .map_err(|_| interrupted("removal_interrupted"))?;
    }
    let name = body["model_name"]
        .as_str()
        .or_else(|| body["model"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "model_name_required",
                "Provide model_name.",
            )
        })?;
    tokio::spawn(async move { delete_model(&manager, &name).await.map(Json) })
        .await
        .map_err(|_| interrupted("removal_interrupted"))?
}

async fn delete_model(manager: &Arc<Manager>, name: &str) -> Result<Value, ApiError> {
    let Some(id) = registered(manager, name).await else {
        let public = public_name(name);
        let pending = registry()
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(public);
        registry().remove(&format!("model:{name}"));
        return match pending {
            Some(_) => Ok(success(format!("Deleted model: {name}"))),
            None => Err(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "model_not_found",
                format!("Model not found: {name}"),
            )),
        };
    };
    let model = manager
        .native_model(&id)
        .await
        .ok_or_else(|| not_found(name))?;
    if model.view.policy != Policy::Disabled {
        manager
            .set_policy(&id, Policy::Disabled, model.view.revision)
            .await?;
    }
    if model.view.loaded {
        manager.unload(&id).await?;
    }
    let current = manager
        .native_model(&id)
        .await
        .ok_or_else(|| not_found(name))?;
    let removal = manager.unregister(&id, current.view.revision).await?;
    let released = manager.release_native_weights(&model).await;
    registry().remove(&format!("model:{name}"));
    let mut body = success(format!("Deleted model: {name}"));
    body["model"] = json!(id);
    body["registration_removed"] = removal["registration_removed"].clone();
    body["weights_removed"] =
        json!(released.removed_artifacts > 0 && released.retained_artifacts == 0);
    body["weights_retained"] =
        json!(released.retained_artifacts > 0 || released.unmanaged_files > 0);
    body["freed_bytes"] = json!(released.freed_bytes);
    Ok(body)
}

const LOAD_KEYS: [&str; 6] = [
    "model_name",
    "pinned",
    "save_options",
    "ctx_size",
    "llamacpp_args",
    "merge_args",
];

fn positive(value: &Value, field: &str) -> Result<Option<u32>, ApiError> {
    match value {
        Value::Null => Ok(None),
        Value::Number(number) if number.as_i64() == Some(-1) => Ok(None),
        Value::Number(number) => number
            .as_u64()
            .filter(|value| *value > 0)
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "invalid_option",
                    format!("{field} must be a positive whole number or -1."),
                )
            }),
        Value::String(text) => positive(
            &text.parse::<i64>().map(Value::from).map_err(|_| {
                error(
                    StatusCode::BAD_REQUEST,
                    "invalid_option",
                    format!("{field} must be a positive whole number or -1."),
                )
            })?,
            field,
        ),
        _ => Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_option",
            format!("{field} must be a positive whole number or -1."),
        )),
    }
}

/// Folds the native load options the rewrite's typed launch options can carry into `saved`.
fn launch_options(
    saved: &LaunchOptions,
    request: &Map<String, Value>,
    backend: Option<&str>,
) -> Result<LaunchOptions, ApiError> {
    let mut options = saved.clone();
    for (key, value) in request {
        if LOAD_KEYS.contains(&key.as_str()) || value.is_null() {
            continue;
        }
        if key == "llamacpp_backend" && value.as_str().is_some_and(|wanted| Some(wanted) == backend)
        {
            continue;
        }
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_load_option",
            format!(
                "Juiced does not apply '{key}' on load. Supported: ctx_size and llamacpp_args with --ctx-size, --threads, --gpu-layers or --parallel. Change the backend with POST /v1/models/{{id}}/runtime."
            ),
        ));
    }
    if request.get("merge_args") == Some(&Value::Bool(false)) {
        options = LaunchOptions::default();
    }
    if let Some(value) = request.get("ctx_size") {
        options.context_tokens = positive(value, "ctx_size")?;
    }
    if let Some(args) = request.get("llamacpp_args") {
        let text = match args {
            Value::Null => "",
            Value::String(text) => text.as_str(),
            _ => {
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "invalid_option",
                    "llamacpp_args must be a string.",
                ));
            }
        };
        let mut tokens = text.split_whitespace();
        while let Some(flag) = tokens.next() {
            let (flag, inline) = match flag.split_once('=') {
                Some((flag, value)) => (flag, Some(value)),
                None => (flag, None),
            };
            let field = crate::profiles::option_field(flag).ok_or_else(|| {
                error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "unsupported_load_option",
                    format!(
                        "Juiced does not pass '{flag}' to llama-server. Supported: --ctx-size, --threads, --gpu-layers and --parallel."
                    ),
                )
            })?;
            let value = inline.or_else(|| tokens.next()).ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "invalid_option",
                    format!("{flag} needs a value."),
                )
            })?;
            let parsed = positive(&Value::String(value.to_owned()), flag)?;
            match field {
                0 => options.context_tokens = parsed,
                1 => options.threads = parsed,
                2 => options.gpu_layers = parsed,
                _ => options.parallel_slots = parsed,
            }
        }
    }
    Ok(options)
}

async fn installed_backend(manager: &Manager, package: Option<&str>) -> Option<String> {
    let package = package?;
    let engines = manager.engines().await.ok()?;
    engines["installed"]
        .as_array()?
        .iter()
        .find(|row| row["package"] == package)
        .and_then(|row| row["release"]["backend"].as_str().map(str::to_owned))
}

pub(crate) async fn native_load(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Response {
    let reply: Result<Response, Response> = async move {
        let body = body_value(&bytes).map_err(IntoResponse::into_response)?;
        if body.get("model_name").is_none() {
            let action: ModelAction = decode(body).map_err(IntoResponse::into_response)?;
            return state
                .manager
                .load(&action.model)
                .await
                .map(|view| Json(json!(view)).into_response())
                .map_err(IntoResponse::into_response);
        }
        let request: Map<String, Value> = decode(body).map_err(IntoResponse::into_response)?;
        let name = request["model_name"]
            .as_str()
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_REQUEST,
                    "model_name_required",
                    "Provide model_name.",
                )
                .into_response()
            })?
            .to_owned();
        let manager = state.manager.clone();
        Ok(
            tokio::spawn(async move { load_model(&manager, &name, &request).await })
                .await
                .unwrap_or_else(|_| interrupted("load_interrupted").into_response()),
        )
    }
    .await;
    reply.unwrap_or_else(|response| response)
}

async fn load_model(manager: &Arc<Manager>, name: &str, request: &Map<String, Value>) -> Response {
    let reply: Result<Response, Response> = async move {
        let id = match registered(manager, name).await {
            Some(id) => id,
            None => {
                let body = PullBody {
                    model_name: name.to_owned(),
                    ..PullBody::default()
                };
                match plan(manager, &body).await {
                    Ok(Plan::Installed(id)) => id,
                    Ok(Plan::Download { public, selection }) => {
                        let job = begin(manager, name, public.clone(), selection);
                        outcome(name, settle(&job).await)
                            .map(drop)
                            .map_err(reject)?;
                        public
                    }
                    Err(_) => return Err(not_found(name).into_response()),
                }
            }
        };
        let model = manager
            .native_model(&id)
            .await
            .ok_or_else(|| not_found(name).into_response())?;
        let saved_view = manager.options(&id).await.map_err(reject)?;
        let saved: LaunchOptions =
            serde_json::from_value(saved_view["saved"].clone()).unwrap_or_default();
        let backend = installed_backend(manager, model.view.runtime_package.as_deref()).await;
        let desired = launch_options(&saved, request, backend.as_deref()).map_err(reject)?;
        // The rewrite keeps one saved launch profile and refuses to rewrite it under a running
        // worker, so load options persist even without save_options.
        let changed = desired != saved;
        if changed {
            let apply = if model.view.loaded {
                OptionsApply::Reload
            } else {
                OptionsApply::NextLoad
            };
            manager
                .apply_options(&id, model.view.revision, desired, apply)
                .await
                .map_err(reject)?;
        }
        let loaded = manager.load(&id).await;
        let mut view = loaded.map_err(reject)?;
        match request.get("pinned").and_then(Value::as_bool) {
            Some(true) if view.policy != Policy::Hot => {
                view = manager
                    .set_policy(&id, Policy::Hot, view.revision)
                    .await
                    .map_err(reject)?;
            }
            Some(false) if view.policy == Policy::Hot => {
                view = manager
                    .set_policy(&id, Policy::OnDemand, view.revision)
                    .await
                    .map_err(reject)?;
            }
            _ => {}
        }
        let mut body = success(format!("Loaded model: {name}"));
        body["options_saved"] = json!(changed);
        body["model"] = json!(view);
        Ok(Json(body).into_response())
    }
    .await;
    reply.unwrap_or_else(|response| response)
}

pub(crate) async fn native_unload(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let body = body_value(&bytes)?;
    if body.get("model").is_some() {
        let action: ModelAction = decode(body)?;
        return Ok(Json(json!(state.manager.unload(&action.model).await?)));
    }
    let name = match &body {
        Value::Null => None,
        Value::Object(object) => match object.get("model_name") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if name.is_empty() => None,
            Some(Value::String(name)) => Some(name.clone()),
            Some(_) => {
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "model_name must be a string.",
                ));
            }
        },
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Provide a JSON object or no body.",
            ));
        }
    };
    let Some(name) = name else {
        for model in state.manager.catalog().await {
            if model.loaded || model.pid.is_some() {
                state.manager.unload(&model.id).await?;
            }
        }
        return Ok(Json(success("All models unloaded successfully".into())));
    };
    let id = registered(&state.manager, &name)
        .await
        .ok_or_else(|| not_found(&name))?;
    let model = state
        .manager
        .native_model(&id)
        .await
        .ok_or_else(|| not_found(&name))?;
    if !model.view.loaded && model.view.pid.is_none() {
        return Err(error(
            StatusCode::NOT_FOUND,
            "model_not_loaded",
            format!("Model not loaded: {name}"),
        ));
    }
    state.manager.unload(&id).await?;
    Ok(Json(success("Model unloaded successfully".into())))
}

// ---------------------------------------------------------------------------------------------
// Model files and options reset.

#[derive(Deserialize)]
pub(crate) struct FilesQuery {
    include_paths: Option<bool>,
}

pub(crate) async fn native_model_files(
    State(state): State<AppState>,
    Extension(caller): Extension<crate::auth::RequestContext>,
    Path(name): Path<String>,
    ApiQuery(query): ApiQuery<FilesQuery>,
) -> Result<Json<Value>, ApiError> {
    let include = query.include_paths.unwrap_or(false);
    if include && !caller.operator() {
        return Err(error(
            StatusCode::FORBIDDEN,
            "paths_require_operator",
            "File paths are shown only to operator credentials.",
        ));
    }
    let id = registered(&state.manager, &name).await.ok_or_else(|| {
        error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("Model '{name}' was not found."),
        )
    })?;
    let mut body = state.manager.native_files(&id, include).await?;
    body["model_id"] = json!(name);
    Ok(Json(body))
}

pub(crate) async fn native_options_reset(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let reply: Result<Response, Response> = async move {
        let id = registered(&state.manager, &name).await.ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("Model '{name}' was not found."),
            )
            .into_response()
        })?;
        let manager = state.manager.clone();
        tokio::spawn(async move {
            let model = manager
                .native_model(&id)
                .await
                .ok_or_else(|| not_found(&id).into_response())?;
            manager
                .apply_options(
                    &id,
                    model.view.revision,
                    LaunchOptions::default(),
                    OptionsApply::NextLoad,
                )
                .await
                .map_err(reject)?;
            manager
                .options(&id)
                .await
                .map(|view| Json(view).into_response())
                .map_err(reject)
        })
        .await
        .map_err(|_| interrupted("options_interrupted").into_response())?
    }
    .await;
    reply.unwrap_or_else(|response| response)
}

// ---------------------------------------------------------------------------------------------
// Engines.

#[derive(Deserialize)]
struct BackendRequest {
    #[serde(default)]
    recipe: Option<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    arch: Option<String>,
}

fn backend_request(bytes: &Bytes) -> Result<(BackendRequest, Engine, String), ApiError> {
    let request: BackendRequest = decode(body_value(bytes)?)?;
    if request.backend.as_deref() == Some("cloud") {
        return Err(error(
            StatusCode::NOT_IMPLEMENTED,
            "cloud_unsupported",
            "Cloud providers are not available in Juiced.",
        ));
    }
    let (Some(recipe), Some(backend)) = (
        request.recipe.clone().filter(|value| !value.is_empty()),
        request.backend.clone().filter(|value| !value.is_empty()),
    ) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "recipe_backend_required",
            "Both 'recipe' and 'backend' are required",
        ));
    };
    let engine = recipe_engine(&recipe)?;
    Ok((request, engine, backend))
}

async fn install(
    manager: &Arc<Manager>,
    engine: Engine,
    backend: &str,
) -> Result<Option<String>, ApiError> {
    let generation = manager.engines().await?["generation"]
        .as_u64()
        .unwrap_or_default();
    let (_, body) = manager
        .install_engine(EngineInstall {
            engine: engine.name().into(),
            backend: backend.into(),
            expected_generation: generation,
        })
        .await?;
    Ok(body["job"]["id"].as_str().map(str::to_owned))
}

fn engine_failure(job: &Value) -> ApiError {
    let code = job["error"].as_str().unwrap_or("engine_install_failed");
    error(
        StatusCode::BAD_GATEWAY,
        "engine_install_failed",
        format!("Backend installation failed ({code})."),
    )
}

pub(crate) async fn native_install(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let (request, engine, backend) = backend_request(&bytes)?;
    let recipe = engine.recipe();
    let started = install(&state.manager, engine, &backend).await;
    let done = json!({"status": "success", "recipe": recipe, "backend": backend});
    if request.stream {
        let job = match started {
            Ok(job) => job,
            Err(failure) => {
                return Ok(sse(futures_util::stream::iter([event(
                    "error",
                    json!({"error": failure.message, "code": failure.code}),
                )])));
            }
        };
        let manager = state.manager.clone();
        let deadline = Instant::now() + INSTALL_DEADLINE;
        let stream = futures_util::stream::unfold(
            (job, None::<u64>, false),
            move |(job, last, done_sent)| {
                let manager = manager.clone();
                let done = done.clone();
                let backend = backend.clone();
                async move {
                    if done_sent {
                        return None;
                    }
                    let Some(id) = job.clone() else {
                        return Some((event("complete", done), (job, last, true)));
                    };
                    loop {
                        let view = manager.engine_job(&id).unwrap_or(Value::Null);
                        match view["state"].as_str() {
                            Some("complete") => {
                                return Some((event("complete", done), (job, last, true)));
                            }
                            Some("failed") | None => {
                                let failure = engine_failure(&view);
                                return Some((
                                    event(
                                        "error",
                                        json!({"error": failure.message, "code": failure.code}),
                                    ),
                                    (job, last, true),
                                ));
                            }
                            _ if Instant::now() >= deadline => {
                                return Some((
                                    event(
                                        "error",
                                        json!({"error": "Backend installation timed out.", "code": "engine_install_timeout"}),
                                    ),
                                    (job, last, true),
                                ));
                            }
                            _ => {}
                        }
                        let downloaded = view["downloaded_bytes"].as_u64().unwrap_or(0);
                        if last != Some(downloaded) {
                            let total = view["total_bytes"].as_u64().unwrap_or(0);
                            let data = json!({
                                "file": format!("{recipe}-{backend}"), "file_index": 1, "total_files": 1,
                                "bytes_downloaded": downloaded, "bytes_total": total,
                                "total_download_size": total,
                                "percent": downloaded.saturating_mul(100).checked_div(total).unwrap_or(0).min(100),
                            });
                            return Some((event("progress", data), (job, Some(downloaded), false)));
                        }
                        tokio::time::sleep(POLL).await;
                    }
                }
            },
        );
        return Ok(sse(stream));
    }
    if let Some(id) = started? {
        let deadline = Instant::now() + INSTALL_DEADLINE;
        loop {
            let view = state.manager.engine_job(&id).unwrap_or(Value::Null);
            match view["state"].as_str() {
                Some("complete") => break,
                Some("failed") | None => return Err(engine_failure(&view)),
                _ if Instant::now() >= deadline => {
                    return Err(error(
                        StatusCode::GATEWAY_TIMEOUT,
                        "engine_install_timeout",
                        "Backend installation timed out.",
                    ));
                }
                _ => tokio::time::sleep(POLL).await,
            }
        }
    }
    Ok(Json(done).into_response())
}

pub(crate) async fn native_install_dry_run(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let (request, engine, backend) = backend_request(&bytes)?;
    let mut body = state
        .manager
        .engine_dry_run(engine, &backend, request.arch.as_deref())
        .await?;
    body["recipe"] = json!(request.recipe);
    Ok(Json(body))
}

pub(crate) async fn native_uninstall(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let (_, engine, backend) = backend_request(&bytes)?;
    let mut body = match state.manager.uninstall_engine(engine, &backend).await {
        Err(failure) if failure.code == "engine_not_installed" => {
            json!({"engine": engine.name(), "backend": backend, "removed_packages": [], "freed_bytes": 0})
        }
        other => other?,
    };
    body["status"] = json!("success");
    body["recipe"] = json!(engine.recipe());
    Ok(Json(body))
}

/// The native `recipes` block, derived from the engine inventory.
fn recipes(engines: &Value, system: &Value) -> Value {
    let mut recipes = Map::new();
    for engine in Engine::all() {
        let recipe = engine.recipe();
        let mut backends = Map::new();
        for row in engines["installable"].as_array().into_iter().flatten() {
            if row["engine"] != engine.name() {
                continue;
            }
            let Some(backend) = row["backend"].as_str() else {
                continue;
            };
            let installed = row["installed_package"].is_string();
            let unsupported = row["availability"] == "unavailable" || row["build"].is_null();
            let state = if installed {
                "installed"
            } else if unsupported {
                "unsupported"
            } else {
                "installable"
            };
            let devices: Vec<Value> = system["backends"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|variant| variant["recipe"] == recipe && variant["backend"] == backend)
                .and_then(|variant| variant["devices"].as_array().cloned())
                .unwrap_or_default();
            let message = match state {
                "installed" => String::new(),
                "installable" => "Backend is supported but not installed.".into(),
                _ => row["reasons"][0]["message"]
                    .as_str()
                    .or_else(|| row["unresolved"].as_str())
                    .unwrap_or("Not supported on this host.")
                    .to_owned(),
            };
            let mut entry = json!({
                "devices": devices, "state": state, "message": message,
                "action": if state == "installable" { format!("lemonade backends install {recipe}:{backend}") } else { String::new() },
                "tier": "core", "format": "native",
            });
            if installed && let Some(tag) = row["build"]["tag"].as_str() {
                entry["version"] = json!(tag);
            }
            backends.insert(backend.to_owned(), entry);
        }
        if backends.is_empty() {
            continue;
        }
        let mut body = json!({"backends": backends});
        if let Some(default) = engines["recommendations"][recipe].as_str() {
            body["default_backend"] = json!(default);
        }
        recipes.insert(recipe.to_owned(), body);
    }
    Value::Object(recipes)
}

pub(crate) async fn native_system_info(
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let bytes = state.manager.system_info().await?;
    let mut body: Value = serde_json::from_slice(&bytes).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "system_info_unavailable",
            "System information could not be represented.",
        )
    })?;
    if let Ok(engines) = state.manager.engines().await {
        body["recipes"] = recipes(&engines, &body);
    }
    if let Some(name) = body["devices"]["cpu"]["name"].as_str() {
        body["Processor"] = json!(name);
    }
    if let Some(bytes) = body["devices"]["memory"]["total_bytes"].as_u64() {
        body["Physical Memory"] = json!(format!("{:.2} GB", bytes as f64 / 1024f64.powi(3)));
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

// ---------------------------------------------------------------------------------------------
// `/internal/*`: fixed paths the native CLI and tray call; operator authority.

async fn internal_aliases(State(state): State<AppState>) -> Json<Value> {
    let view = state.manager.aliases_view().await;
    let catalog = state.manager.catalog().await;
    let mut aliases = Vec::new();
    for entry in view["aliases"].as_array().into_iter().flatten() {
        let target = entry["target"].as_str().unwrap_or_default();
        let model = catalog.iter().find(|model| model.id == target);
        let recipe = match state.manager.native_model(target).await {
            Some(model) => model.recipe,
            None => None,
        };
        aliases.push(json!({
            "alias": entry["alias"], "target": target,
            "downloaded": model.is_some_and(|model| model.downloaded), "recipe": recipe,
        }));
    }
    Json(json!({"aliases": aliases}))
}

async fn internal_alias_set(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let body = body_value(&bytes)?;
    let alias = body["alias"].as_str().filter(|alias| !alias.is_empty());
    let target = body["target"]
        .as_str()
        .or_else(|| body["model"].as_str())
        .filter(|target| !target.is_empty());
    let (Some(alias), Some(target)) = (alias, target) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_alias",
            "Both 'alias' and 'target' are required.",
        ));
    };
    let id = registered(&state.manager, target).await.ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "unknown_model",
            format!("Model not found: {target}"),
        )
    })?;
    let revision = state.manager.aliases_view().await["revision"]
        .as_u64()
        .unwrap_or_default();
    let manager = state.manager.clone();
    let change = crate::supervisor::AliasChange {
        alias: alias.to_owned(),
        target: Some(id),
        expected_revision: revision,
    };
    tokio::spawn(async move { manager.set_alias(change).await })
        .await
        .map_err(|_| interrupted("alias_interrupted"))??;
    Ok(Json(
        json!({"status": "ok", "alias": alias, "target": target}),
    ))
}

async fn internal_alias_delete(
    State(state): State<AppState>,
    Path(alias): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let revision = state.manager.aliases_view().await["revision"]
        .as_u64()
        .unwrap_or_default();
    let manager = state.manager.clone();
    let change = crate::supervisor::AliasChange {
        alias: alias.clone(),
        target: None,
        expected_revision: revision,
    };
    match tokio::spawn(async move { manager.set_alias(change).await })
        .await
        .map_err(|_| interrupted("alias_interrupted"))?
    {
        Ok(_) => Ok(Json(json!({"status": "deleted", "alias": alias}))),
        Err(failure) if failure.status == StatusCode::NOT_FOUND => Err(error(
            StatusCode::NOT_FOUND,
            "alias_not_found",
            format!("Alias not found: {alias}"),
        )),
        Err(failure) => Err(failure),
    }
}

async fn internal_pin(
    State(state): State<AppState>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Value>, ApiError> {
    let body = body_value(&bytes)?;
    let name = body["model_name"].as_str().unwrap_or_default().to_owned();
    let pinned = body["pinned"].as_bool().unwrap_or(true);
    let id = registered(&state.manager, &name).await.ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "model_not_found",
            format!("Model not found: {name}"),
        )
    })?;
    let manager = state.manager.clone();
    tokio::spawn(async move {
        let model = manager
            .native_model(&id)
            .await
            .ok_or_else(|| not_found(&id))?;
        let policy = match (pinned, model.view.policy) {
            (true, _) => Policy::Hot,
            (false, Policy::Hot) => Policy::OnDemand,
            (false, policy) => policy,
        };
        if policy != model.view.policy {
            manager.set_policy(&id, policy, model.view.revision).await?;
        }
        Ok(Json(
            json!({"status": "success", "model_name": name, "pinned": pinned, "policy": policy}),
        ))
    })
    .await
    .map_err(|_| interrupted("pin_interrupted"))?
}

async fn internal_shutdown() -> Result<Json<Value>, ApiError> {
    #[cfg(unix)]
    {
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            // SAFETY: signalling this process is the documented graceful-shutdown path.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGTERM);
            }
        });
        Ok(Json(json!({"status": "shutting_down"})))
    }
    #[cfg(not(unix))]
    {
        Err(error(
            StatusCode::NOT_IMPLEMENTED,
            "shutdown_unsupported",
            "Stop the service through its supervisor on this platform.",
        ))
    }
}

pub(crate) fn internal_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/internal/aliases",
            get(internal_aliases).post(internal_alias_set),
        )
        .route("/internal/aliases/{alias}", delete(internal_alias_delete))
        .route("/internal/pin", post(internal_pin))
        .route("/internal/shutdown", post(internal_shutdown))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_names_strip_only_known_namespaces() {
        assert_eq!(public_name("user.Tiny"), "Tiny");
        assert_eq!(public_name("extra.Tiny"), "Tiny");
        assert_eq!(public_name("user."), "user.");
        assert_eq!(public_name("Qwen3-0.6B-GGUF"), "Qwen3-0.6B-GGUF");
    }

    #[test]
    fn load_options_map_onto_typed_launch_options() {
        let request: Map<String, Value> = serde_json::from_value(json!({
            "model_name": "m", "ctx_size": 8192, "save_options": true,
            "llamacpp_args": "-np 2 --threads=4 -ngl 99",
        }))
        .unwrap();
        let options = launch_options(&LaunchOptions::default(), &request, None).unwrap();
        assert_eq!(options.context_tokens, Some(8192));
        assert_eq!(options.parallel_slots, Some(2));
        assert_eq!(options.threads, Some(4));
        assert_eq!(options.gpu_layers, Some(99));
        let auto: Map<String, Value> =
            serde_json::from_value(json!({"model_name": "m", "ctx_size": -1})).unwrap();
        let saved = LaunchOptions {
            context_tokens: Some(4096),
            ..LaunchOptions::default()
        };
        assert_eq!(
            launch_options(&saved, &auto, None).unwrap().context_tokens,
            None
        );
    }

    #[test]
    fn unsupported_load_options_are_refused_not_ignored() {
        for body in [
            json!({"model_name": "m", "llamacpp_args": "--flash-attn on"}),
            json!({"model_name": "m", "steps": 4}),
            json!({"model_name": "m", "llamacpp_backend": "rocm"}),
        ] {
            let request: Map<String, Value> = serde_json::from_value(body).unwrap();
            let failure =
                launch_options(&LaunchOptions::default(), &request, Some("vulkan")).unwrap_err();
            assert_eq!(failure.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        let same: Map<String, Value> = serde_json::from_value(
            json!({"model_name": "m", "llamacpp_backend": "vulkan", "whispercpp_args": null}),
        )
        .unwrap();
        assert!(launch_options(&LaunchOptions::default(), &same, Some("vulkan")).is_ok());
    }

    #[test]
    fn library_progress_maps_to_native_file_progress() {
        let view = json!({"files": [
            {"file": "dir/a.gguf", "bytes": 100, "state": "present"},
            {"file": "b.gguf", "bytes": 200, "state": "running", "downloaded_bytes": 50},
        ]});
        let mut progress = Progress::default();
        observe(&view, &mut progress);
        assert_eq!(progress.file, "b.gguf");
        assert_eq!(progress.file_index, 2);
        assert_eq!(progress.total_files, 2);
        assert_eq!(progress.total_download_size, 300);
        assert_eq!(progress.completed_files_bytes, 100);
        assert_eq!(progress.cumulative_bytes_downloaded, 150);
        assert_eq!(progress.percent, 25);
    }

    #[test]
    fn empty_and_null_bodies_mean_no_request_fields() {
        assert_eq!(body_value(&Bytes::new()).unwrap(), Value::Null);
        assert_eq!(
            body_value(&Bytes::from_static(b"null")).unwrap(),
            Value::Null
        );
        assert!(body_value(&Bytes::from_static(b"{")).is_err());
    }
}
