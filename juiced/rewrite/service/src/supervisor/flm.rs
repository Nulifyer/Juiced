use super::*;
use crate::engines::{ChoiceError, catalog::Engine};
use serde_json::{Value, json};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

const RETAINED_JOBS: usize = 16;
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const PULL_TIMEOUT: Duration = Duration::from_secs(3600);
const HELPER_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 16 * 1024;
const INHERITED: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

#[derive(Clone, Serialize)]
struct PullStatus {
    state: &'static str,
    percent: Option<u8>,
    error: Option<&'static str>,
    exit_code: Option<i32>,
    finished_at_unix_ms: Option<u64>,
}

struct PullJob {
    id: String,
    checkpoint: String,
    package: String,
    started_at_unix_ms: u64,
    status: Mutex<PullStatus>,
}

impl PullJob {
    fn update(&self, change: impl FnOnce(&mut PullStatus)) {
        change(
            &mut self
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
    }
    fn view(&self) -> Value {
        let status = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        json!({
            "id": self.id, "checkpoint": self.checkpoint, "package": self.package,
            "state": status.state, "percent": status.percent, "error": status.error,
            "exit_code": status.exit_code, "started_at_unix_ms": self.started_at_unix_ms,
            "finished_at_unix_ms": status.finished_at_unix_ms,
        })
    }
    fn active(&self) -> bool {
        !matches!(
            self.status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .state,
            "complete" | "failed"
        )
    }
}

#[derive(Default)]
pub struct FlmJobs {
    jobs: Mutex<VecDeque<Arc<PullJob>>>,
}

impl FlmJobs {
    fn views(&self) -> Vec<Value> {
        self.jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|job| job.view())
            .collect()
    }
    fn begin(&self, checkpoint: &str, package: &str) -> Result<Arc<PullJob>, ApiError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
        if jobs.iter().any(|job| job.active()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "flm_pull_busy",
                "Another FLM model pull is running. Wait for it to finish before starting another.",
            ));
        }
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "flm_job_identity",
                "A pull identity could not be generated.",
            )
        })?;
        let job = Arc::new(PullJob {
            id: format!(
                "flm-{}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            checkpoint: checkpoint.to_owned(),
            package: package.to_owned(),
            started_at_unix_ms: unix_time_ms(),
            status: Mutex::new(PullStatus {
                state: "pulling",
                percent: None,
                error: None,
                exit_code: None,
                finished_at_unix_ms: None,
            }),
        });
        if jobs.len() == RETAINED_JOBS {
            jobs.pop_front();
        }
        jobs.push_back(job.clone());
        Ok(job)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlmPull {
    pub checkpoint: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlmModelAdd {
    pub checkpoint: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub policy: Option<Policy>,
    #[serde(default)]
    pub context_tokens: Option<u32>,
}

struct Runtime {
    package: String,
    executable: PathBuf,
    environment: Vec<(String, String)>,
    _lease: Arc<crate::runtime_packages::Lease>,
}

fn invalid(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn helper_failed(code: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        code,
        "The FLM helper command failed. Inspect GET /juiced/flm and the service log.",
    )
}

fn checkpoint(value: &str) -> Result<(), ApiError> {
    if crate::flm::valid_checkpoint(value) {
        Ok(())
    } else {
        Err(invalid(
            "invalid_checkpoint",
            "FLM checkpoints are name:tag literals such as gemma3:4b.",
        ))
    }
}

impl Manager {
    fn flm_store(&self) -> PathBuf {
        crate::flm::store(self.config_path.parent().unwrap_or(Path::new(".")))
    }

    /// Selects the installed FLM package exactly like automatic engine registration, so a
    /// host whose NPU detection reports the backend unavailable gets the same refusal.
    async fn flm_runtime(&self) -> Result<Runtime, ApiError> {
        let snapshot = self.hardware.snapshot().await.map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "hardware_detection_unavailable",
                "Hardware detection did not complete. Retry later.",
            )
        })?;
        let inventory = crate::hardware::project(&self.runtime_definitions, &snapshot.report);
        let packages = self.runtime_packages().await?.value;
        let installed = crate::engines::installed_packages(&packages);
        let choice = crate::engines::choose(
            Engine::FastFlowLm,
            &inventory,
            &snapshot.report,
            &installed,
            Some("npu"),
        )
        .map_err(|error| match error {
            ChoiceError::NotInstalled { .. } => ApiError::new(
                StatusCode::CONFLICT,
                "flm_not_installed",
                "Install the fastflowlm npu engine first with POST /juiced/engines/install.",
            ),
            _ => ApiError::new(
                StatusCode::CONFLICT,
                "no_usable_backend",
                "Hardware detection reports no usable AMD NPU for FLM. Review GET /system-info.",
            ),
        })?;
        let store = self.runtime_packages.get().cloned().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "package_store_unavailable",
                "The runtime package store is unavailable.",
            )
        })?;
        let package = choice.package.clone();
        tokio::task::spawn_blocking(move || {
            let lease = store
                .hold(
                    &package,
                    crate::config::Provider::FastFlowLm,
                    std::time::Instant::now() + Duration::from_secs(60),
                )
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "runtime_package_unavailable",
                        "The installed FLM package could not be verified.",
                    )
                })?;
            let bundle = lease.bundle();
            Ok(Runtime {
                package,
                executable: bundle.root.join(&bundle.entrypoint),
                environment: crate::flm::runtime_environment(&bundle.root),
                _lease: lease,
            })
        })
        .await
        .map_err(|_| helper_failed("flm_helper_interrupted"))?
    }

    fn flm_command(&self, runtime: &Runtime, arguments: &[String]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&runtime.executable);
        command.args(arguments).env_clear();
        for name in INHERITED {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .envs(runtime.environment.iter().map(|(key, value)| (key, value)))
            .env(crate::flm::MODEL_PATH_ENV, self.flm_store())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn flm_output(
        &self,
        runtime: &Runtime,
        arguments: &[&str],
    ) -> Result<(Option<i32>, String), &'static str> {
        let arguments: Vec<String> = arguments.iter().map(|value| (*value).to_owned()).collect();
        let mut child = self
            .flm_command(runtime, &arguments)
            .spawn()
            .map_err(|_| "flm_spawn_failed")?;
        let mut stdout = child.stdout.take().ok_or("flm_spawn_failed")?;
        let read = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(HELPER_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| "flm_output_invalid")?;
            if bytes.len() > HELPER_OUTPUT_BYTES {
                return Err("flm_output_limit");
            }
            let status = child.wait().await.map_err(|_| "flm_spawn_failed")?;
            Ok((
                status.code(),
                String::from_utf8(bytes).map_err(|_| "flm_output_invalid")?,
            ))
        };
        tokio::time::timeout(HELPER_TIMEOUT, read)
            .await
            .map_err(|_| "flm_timed_out")?
    }

    async fn flm_list(&self, runtime: &Runtime) -> Result<Vec<crate::flm::ListedModel>, ApiError> {
        let (code, output) = self
            .flm_output(runtime, &["list", "--json", "--quiet"])
            .await
            .map_err(helper_failed)?;
        if code != Some(0) {
            return Err(helper_failed("flm_list_failed"));
        }
        crate::flm::parse_list(&output).map_err(helper_failed)
    }

    pub async fn flm_status(&self) -> Result<Value, ApiError> {
        let store = self.flm_store();
        let runtime = match self.flm_runtime().await {
            Ok(runtime) => runtime,
            Err(error) => {
                return Ok(json!({
                    "schema_version": 1, "engine": "fastflowlm", "package": null,
                    "store": store, "available": false,
                    "unavailable": {"code": error.code, "message": error.message},
                    "validation": null, "models": [], "list_error": null,
                    "jobs": self.flm_jobs.views(), "compatibility": "unqualified",
                }));
            }
        };
        let validation = self
            .flm_output(&runtime, &["validate", "--json"])
            .await
            .ok()
            .map(|(_, output)| crate::flm::parse_validation(&output));
        let registered: Vec<(String, String)> = self
            .config
            .lock()
            .await
            .models
            .iter()
            .filter(|model| model.execution_profile.adapter == crate::config::Provider::FastFlowLm)
            .filter_map(|model| {
                crate::flm::checkpoint(&model.execution_profile.args)
                    .map(|checkpoint| (checkpoint.to_owned(), model.id.clone()))
            })
            .collect();
        let (models, list_error) = match self.flm_list(&runtime).await {
            Ok(models) => (models, None),
            Err(error) => (Vec::new(), Some(error.code)),
        };
        let models: Vec<Value> = models
            .into_iter()
            .map(|model| {
                let ids: Vec<&str> = registered
                    .iter()
                    .filter(|(checkpoint, _)| *checkpoint == model.checkpoint)
                    .map(|(_, id)| id.as_str())
                    .collect();
                let mut view = serde_json::to_value(&model).unwrap_or_default();
                view["registered"] = json!(ids);
                view
            })
            .collect();
        Ok(json!({
            "schema_version": 1, "engine": "fastflowlm", "package": runtime.package,
            "store": store, "available": true, "unavailable": null,
            "validation": validation, "models": models, "list_error": list_error,
            "jobs": self.flm_jobs.views(), "compatibility": "unqualified",
        }))
    }

    pub async fn flm_pull(self: &Arc<Self>, request: FlmPull) -> Result<Value, ApiError> {
        checkpoint(&request.checkpoint)?;
        if self.paused.load(Ordering::Acquire) || self.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_import_paused",
                "FLM pulls are unavailable while new work is paused or the service is stopping.",
            ));
        }
        if self.config.lock().await.artifact_source_policy
            == crate::artifacts::SourcePolicy::Offline
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "engine_source_offline",
                "The artifact source policy is offline; FLM model downloads are disabled.",
            ));
        }
        let runtime = self.flm_runtime().await?;
        let job = self.flm_jobs.begin(&request.checkpoint, &runtime.package)?;
        let store = self.flm_store();
        let manager = self.clone();
        let running = job.clone();
        tokio::spawn(async move {
            let result = async {
                tokio::fs::create_dir_all(&store)
                    .await
                    .map_err(|_| ("flm_store_unavailable", None))?;
                let arguments = crate::flm::pull_arguments(&running.checkpoint, request.force);
                let mut child = manager
                    .flm_command(&runtime, &arguments)
                    .spawn()
                    .map_err(|_| ("flm_spawn_failed", None))?;
                let stdout = child.stdout.take().ok_or(("flm_spawn_failed", None))?;
                let follow = async {
                    let mut lines = tokio::io::BufReader::new(stdout);
                    let mut line = Vec::new();
                    loop {
                        line.clear();
                        let read = (&mut lines)
                            .take(MAX_LINE_BYTES as u64)
                            .read_until(b'\n', &mut line)
                            .await
                            .map_err(|_| ("flm_output_invalid", None))?;
                        if read == 0 {
                            break;
                        }
                        if manager.shutting_down.load(Ordering::Acquire) {
                            return Err(("cancelled", None));
                        }
                        if let Some(percent) =
                            crate::flm::progress_percent(&String::from_utf8_lossy(&line))
                        {
                            running.update(|status| status.percent = Some(percent));
                        }
                    }
                    let status = child.wait().await.map_err(|_| ("flm_spawn_failed", None))?;
                    if status.success() {
                        Ok(())
                    } else {
                        Err(("flm_pull_failed", status.code()))
                    }
                };
                tokio::time::timeout(PULL_TIMEOUT, follow)
                    .await
                    .map_err(|_| ("flm_timed_out", None))?
            }
            .await;
            running.update(|status| {
                match result {
                    Ok(()) => {
                        status.state = "complete";
                        status.percent = Some(100);
                    }
                    Err((code, exit)) => {
                        status.state = "failed";
                        status.error = Some(code);
                        status.exit_code = exit;
                    }
                }
                status.finished_at_unix_ms = Some(unix_time_ms());
            });
        });
        Ok(json!({"job": job.view()}))
    }

    pub async fn flm_add_model(self: &Arc<Self>, request: FlmModelAdd) -> Result<Value, ApiError> {
        checkpoint(&request.checkpoint)?;
        if request
            .context_tokens
            .is_some_and(|context| context == 0 || context > i32::MAX as u32)
        {
            return Err(invalid(
                "invalid_launch_options",
                "Context must be 1 to 2147483647 tokens.",
            ));
        }
        let id = request
            .id
            .clone()
            .unwrap_or_else(|| crate::flm::default_id(&request.checkpoint));
        if id.trim().is_empty()
            || id.trim() != id
            || id.len() > 256
            || id.contains(',')
            || id.chars().any(char::is_control)
        {
            return Err(invalid(
                "invalid_model_id",
                "Model IDs must be nonempty literal text of at most 256 bytes without commas or surrounding whitespace.",
            ));
        }
        let runtime = self.flm_runtime().await?;
        let listed = self.flm_list(&runtime).await?;
        let model = listed
            .into_iter()
            .find(|model| model.checkpoint == request.checkpoint)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "flm_model_unknown",
                    "The installed FLM release does not list this checkpoint.",
                )
            })?;
        if !model.installed {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "flm_model_not_installed",
                "Pull this checkpoint first with POST /juiced/flm/pull.",
            ));
        }
        if model.kind != "chat" {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "flm_model_unsupported",
                "Only FLM chat checkpoints can be registered; embedding and transcription modes are not supported yet.",
            ));
        }
        let store = self.flm_store();
        let directory = model.repo_directory.clone().ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "flm_model_files_missing",
                "FLM did not report where this checkpoint is stored.",
            )
        })?;
        let config = crate::flm::config_path(&store, &directory);
        let footprint = model.footprint_gb;
        let (config, resident_bytes) = tokio::task::spawn_blocking(move || {
            let metadata = std::fs::symlink_metadata(&config).ok()?;
            if !metadata.is_file() {
                return None;
            }
            let bytes = match footprint {
                Some(gigabytes) if gigabytes > 0.0 => (gigabytes * 1024f64.powi(3)).ceil() as u64,
                _ => directory_bytes(config.parent()?)?,
            };
            Some((config, bytes.max(1)))
        })
        .await
        .map_err(|_| helper_failed("flm_helper_interrupted"))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "flm_model_files_missing",
                "The pulled checkpoint has no config.json in the FLM store. Pull it again.",
            )
        })?;
        let arguments = crate::flm::serve_arguments(&request.checkpoint, request.context_tokens);
        let registration = json!({
            "id": id,
            "model_file": config,
            "execution_profile": {
                "schema_version": 1,
                "adapter": crate::config::Provider::FastFlowLm,
                "implementation_id": "fastflowlm",
                "runtime_package": runtime.package,
                "args": arguments,
                "options": {},
                "placement": "unresolved",
            },
            "resident_bytes": resident_bytes,
            "max_inflight": 1,
            "operations": ["chat/completions", "completions"],
            "policy": "disabled",
        });
        let crate::config::Registration(spec) = serde_json::from_value(registration.clone())
            .map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "automatic_profile_invalid",
                    "The automatically built execution profile was rejected by the registration schema.",
                )
            })?;
        let mut view = self.register(spec).await?;
        if let Some(policy) = request.policy.filter(|policy| *policy != Policy::Disabled) {
            view = self.set_policy(&id, policy, view.revision).await?;
        }
        Ok(json!({
            "schema_version": 1,
            "model": view,
            "engine": "fastflowlm",
            "backend": "npu",
            "package": runtime.package,
            "checkpoint": request.checkpoint,
            "execution_profile": registration["execution_profile"],
            "resident_bytes": resident_bytes,
            "compatibility": "unqualified",
        }))
    }
}

fn directory_bytes(root: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut pending = vec![root.to_owned()];
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(directory).ok()? {
            entries += 1;
            if entries > 16384 {
                return None;
            }
            let item = item.ok()?;
            let metadata = item.path().symlink_metadata().ok()?;
            if metadata.is_dir() {
                pending.push(item.path());
            } else if metadata.is_file() {
                total = total.checked_add(metadata.len())?;
            }
        }
    }
    Some(total)
}
