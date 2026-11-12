use super::*;
use crate::engines::{
    ChoiceError,
    archive::{self, ArchiveError},
    catalog::{self, Build, Engine},
    download::{self, DownloadError},
    models,
};
use axum::response::IntoResponse;
use serde_json::{Value, json};

const RETAINED_JOBS: usize = 16;
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const STAGING: &str = "engine-staging";

#[derive(Clone, Serialize)]
struct JobStatus {
    state: &'static str,
    error: Option<&'static str>,
    package: Option<String>,
    archive_sha256: Option<String>,
    digest_source: Option<&'static str>,
    total_bytes: Option<u64>,
    finished_at_unix_ms: Option<u64>,
}

pub(super) struct EngineJob {
    id: String,
    build: Build,
    started_at_unix_ms: u64,
    downloaded: AtomicU64,
    runtime_downloaded: AtomicU64,
    status: Mutex<JobStatus>,
}

impl EngineJob {
    fn update(&self, change: impl FnOnce(&mut JobStatus)) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        change(&mut status);
    }
    fn view(&self) -> Value {
        let status = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        json!({
            "id": self.id, "engine": self.build.engine, "backend": self.build.backend,
            "gpu_target": self.build.gpu_target, "repository": self.build.repository,
            "tag": self.build.tag, "asset": self.build.asset, "state": status.state,
            "runtime": self.build.runtime.map(|runtime| json!({
                "name": runtime.name, "version": runtime.version, "asset": runtime.asset,
                "archive_bytes": runtime.archive_bytes, "archive_sha256": runtime.sha256,
            })),
            "downloaded_bytes": self.downloaded.load(Ordering::Acquire)
                + self.runtime_downloaded.load(Ordering::Acquire),
            "total_bytes": status.total_bytes, "archive_sha256": status.archive_sha256,
            "digest_source": status.digest_source, "package": status.package,
            "error": status.error, "started_at_unix_ms": self.started_at_unix_ms,
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
pub struct EngineJobs {
    jobs: Mutex<VecDeque<Arc<EngineJob>>>,
}

impl EngineJobs {
    pub(super) fn views(&self) -> Vec<Value> {
        self.jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|job| job.view())
            .collect()
    }
    fn begin(&self, build: Build) -> Result<Arc<EngineJob>, ApiError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
        if jobs.iter().any(|job| job.active()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "engine_install_busy",
                "Another engine installation is running. Wait for it to finish before starting another.",
            ));
        }
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "engine_job_identity",
                "An installation identity could not be generated.",
            )
        })?;
        let job = Arc::new(EngineJob {
            id: format!(
                "engine-{}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            build,
            started_at_unix_ms: unix_time_ms(),
            downloaded: AtomicU64::new(0),
            runtime_downloaded: AtomicU64::new(0),
            status: Mutex::new(JobStatus {
                state: "queued",
                error: None,
                package: None,
                archive_sha256: None,
                digest_source: None,
                total_bytes: Some(
                    build.archive_bytes + build.runtime.map_or(0, |runtime| runtime.archive_bytes),
                ),
                finished_at_unix_ms: None,
            }),
        });
        while jobs.len() >= RETAINED_JOBS {
            jobs.pop_front();
        }
        jobs.push_back(job.clone());
        Ok(job)
    }
}

pub enum AddModelError {
    Api(ApiError),
    NotInstalled(Value),
}

impl From<ApiError> for AddModelError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl IntoResponse for AddModelError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::NotInstalled(body) => (StatusCode::CONFLICT, axum::Json(body)).into_response(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineInstall {
    pub engine: String,
    pub backend: String,
    pub expected_generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAdd {
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default)]
    pub artifact_set: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub policy: Option<Policy>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

fn invalid(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn archive_failure(error: ArchiveError) -> &'static str {
    error.code()
}

impl Manager {
    pub(super) async fn host(
        &self,
    ) -> Result<(Arc<crate::hardware::Snapshot>, crate::hardware::Inventory), ApiError> {
        let snapshot = self.hardware.snapshot().await.map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "hardware_detection_unavailable",
                "Hardware detection did not complete. Retry later.",
            )
        })?;
        let inventory = crate::hardware::project(&self.runtime_definitions, &snapshot.report);
        Ok((snapshot, inventory))
    }

    pub(super) async fn package_inventory(&self) -> Result<Value, ApiError> {
        let reply = self.runtime_packages().await?;
        Ok(reply.value)
    }

    pub async fn engines(&self) -> Result<Value, ApiError> {
        let (snapshot, inventory) = self.host().await?;
        let packages = self.package_inventory().await?;
        let installed = crate::engines::installed_packages(&packages);
        let mut usage: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for model in &self.config.lock().await.models {
            let profile = &model.execution_profile;
            if let Some(package) = profile.runtime_package() {
                usage
                    .entry(package.to_owned())
                    .or_default()
                    .push(json!({"model": model.id, "selection": "current"}));
            }
            if let Some(package) = profile.previous_runtime_package() {
                usage
                    .entry(package.to_owned())
                    .or_default()
                    .push(json!({"model": model.id, "selection": "previous"}));
            }
        }
        let installed_view: Vec<Value> = packages["packages"]
            .as_array()
            .map(|packages| {
                packages
                    .iter()
                    .map(|package| {
                        let id = package["id"].as_str().unwrap_or_default();
                        json!({
                            "package": id,
                            "phase": package["phase"],
                            "engine": package["implementation_id"].as_str().and_then(Engine::parse),
                            "release": package["provenance"]["release"],
                            "declared_target": package["declared_target"],
                            "declared_host_match": package["declared_host_match"],
                            "reserved_bytes": package["reserved_bytes"],
                            "binding_leases": package["binding_leases"],
                            "models": usage.get(id).cloned().unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let report = &snapshot.report;
        let recommendations: BTreeMap<_, _> = Engine::ALL
            .into_iter()
            .map(|engine| {
                (
                    engine.recipe(),
                    crate::engines::recommended(engine, &inventory, &snapshot.report)
                        .map(|recommendation| recommendation.backend),
                )
            })
            .collect();
        Ok(json!({
            "schema_version": 1,
            "host": {"os": report.host.os, "architecture": report.host.architecture, "detection": report.host.detection},
            "generation": packages["generation"],
            "installation_active": packages["installation_active"],
            "recommendations": recommendations,
            "installable": crate::engines::installable(&inventory, report, &installed),
            "installed": installed_view,
            "jobs": self.engine_jobs.views(),
            "compatibility": "unqualified",
        }))
    }

    pub async fn install_engine(
        self: &Arc<Self>,
        request: EngineInstall,
    ) -> Result<(StatusCode, Value), ApiError> {
        let engine = Engine::parse(&request.engine).ok_or_else(|| {
            invalid(
                "unknown_engine",
                "Choose engine llama.cpp, whisper.cpp, sd.cpp, kokoros, parakeet.cpp, moonshine, futo-transcribe.cpp or fastflowlm.",
            )
        })?;
        if catalog::UNPUBLISHED.contains(&(engine, request.backend.as_str())) {
            return Err(not_published());
        }
        if !catalog::BUILDS
            .iter()
            .any(|build| build.engine == engine && build.backend == request.backend)
        {
            return Err(invalid(
                "unknown_backend",
                "This engine has no installable build for that backend.",
            ));
        }
        if self.paused.load(Ordering::Acquire) || self.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_import_paused",
                "Engine installation is unavailable while new work is paused or the service is stopping.",
            ));
        }
        let (snapshot, inventory) = self.host().await?;
        let build = catalog::resolve(engine, &request.backend, &snapshot.report).map_err(|reason| {
            ApiError::new(
                StatusCode::CONFLICT,
                "engine_build_unavailable",
                match reason {
                    catalog::Unresolved::HostUnsupported => "No build of this engine backend is published for this operating system and CPU architecture.",
                    catalog::Unresolved::NoMatchingGpu => "No build of this engine backend matches a detected GPU target.",
                    catalog::Unresolved::BackendUnknown | catalog::Unresolved::NotPublished => "This engine has no installable build for that backend.",
                    catalog::Unresolved::NoMatchingNpu => "This engine backend needs an AMD XDNA 2 NPU, and none was detected.",
                },
            )
        })?;
        if inventory.backends.iter().any(|variant| {
            variant.recipe == engine.recipe()
                && variant.backend == request.backend
                && variant.state == crate::hardware::Availability::Unavailable
        }) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "backend_unavailable",
                "Hardware detection reports this backend as unavailable on this host. Review GET /system-info.",
            ));
        }
        let policy = self.config.lock().await.artifact_source_policy;
        if policy == crate::artifacts::SourcePolicy::Offline {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "engine_source_offline",
                "The artifact source policy is offline; engine downloads are disabled.",
            ));
        }
        let packages = self.package_inventory().await?;
        if packages["generation"].as_u64() != Some(request.expected_generation) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "manifest_revision_conflict",
                "Installed runtimes changed. Refresh GET /juiced/engines before retrying.",
            ));
        }
        let installed = crate::engines::installed_packages(&packages);
        if let Some(existing) =
            crate::engines::installable(&inventory, &snapshot.report, &installed)
                .into_iter()
                .find(|row| row.engine == engine && row.backend == build.backend)
                .and_then(|row| row.installed_package)
        {
            return Ok((
                StatusCode::OK,
                json!({"job": null, "already_installed": true, "package": existing, "build": build}),
            ));
        }
        let job = self.engine_jobs.begin(build)?;
        let manager = self.clone();
        let running = job.clone();
        let expected = request.expected_generation;
        tokio::spawn(async move {
            let result = manager.run_install(&running, policy, expected).await;
            if result.is_ok() {
                manager.resume_native_adoption();
            }
            running.update(|status| {
                match result {
                    Ok(package) => {
                        status.state = "complete";
                        status.package = Some(package);
                    }
                    Err(code) => {
                        status.state = "failed";
                        status.error = Some(code);
                    }
                }
                status.finished_at_unix_ms = Some(unix_time_ms());
            });
        });
        Ok((StatusCode::ACCEPTED, json!({"job": job.view()})))
    }

    async fn run_install(
        self: &Arc<Self>,
        job: &Arc<EngineJob>,
        policy: crate::artifacts::SourcePolicy,
        expected_generation: u64,
    ) -> Result<String, &'static str> {
        let build = job.build;
        let releases = &self.engine_releases;
        job.update(|status| status.state = "resolving");
        let (sha256, bytes, source) = match build.sha256.filter(|_| releases.pinned_digests) {
            Some(sha256) => (sha256.to_owned(), build.archive_bytes, "pinned"),
            None => {
                let (sha256, bytes) = download::published_digest(
                    &build.release_api_url(&releases.api),
                    build.asset,
                    policy,
                )
                .await
                .map_err(DownloadError::code)?;
                (sha256, bytes, "github-release-api")
            }
        };
        if bytes == 0 || bytes > MAX_ARCHIVE_BYTES {
            return Err("engine_size_limit");
        }
        job.update(|status| {
            status.state = "downloading";
            status.archive_sha256 = Some(sha256.clone());
            status.digest_source = Some(source);
            status.total_bytes =
                Some(bytes + build.runtime.map_or(0, |runtime| runtime.archive_bytes));
        });
        let staging = self
            .config_path
            .parent()
            .ok_or("engine_storage")?
            .join(STAGING)
            .join(&job.id);
        let work = staging.clone();
        tokio::task::spawn_blocking(move || {
            // Installs are serialized, so any existing staging tree was left by an
            // interrupted earlier process.
            if let Some(abandoned) = work.parent() {
                match std::fs::remove_dir_all(abandoned) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(error);
                    }
                    _ => {}
                }
            }
            std::fs::create_dir_all(work.join("tree"))
        })
        .await
        .map_err(|_| "engine_storage")?
        .map_err(|_| "engine_storage")?;
        let result = self
            .stage_and_import(
                job,
                policy,
                &staging,
                (&sha256, bytes, source),
                expected_generation,
            )
            .await;
        let cleanup = staging.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_dir_all(&cleanup);
            if let Some(parent) = cleanup.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        })
        .await;
        result
    }

    async fn stage_and_import(
        self: &Arc<Self>,
        job: &Arc<EngineJob>,
        policy: crate::artifacts::SourcePolicy,
        staging: &std::path::Path,
        (sha256, bytes, source): (&str, u64, &'static str),
        expected_generation: u64,
    ) -> Result<String, &'static str> {
        let build = job.build;
        let archive_path = staging.join("archive");
        let manager = Arc::downgrade(self);
        download::fetch(
            &build.download_url(&self.engine_releases.downloads),
            policy,
            sha256,
            bytes,
            &archive_path,
            &job.downloaded,
            move || {
                manager
                    .upgrade()
                    .is_none_or(|manager| manager.shutting_down.load(Ordering::Acquire))
            },
        )
        .await
        .map_err(DownloadError::code)?;
        let runtime_path = staging.join("runtime");
        if let Some(runtime) = build.runtime {
            if runtime.archive_bytes > MAX_ARCHIVE_BYTES {
                return Err("engine_size_limit");
            }
            let manager = Arc::downgrade(self);
            download::fetch(
                &runtime.download_url(&self.engine_releases.runtimes),
                policy,
                runtime.sha256,
                runtime.archive_bytes,
                &runtime_path,
                &job.runtime_downloaded,
                move || {
                    manager
                        .upgrade()
                        .is_none_or(|manager| manager.shutting_down.load(Ordering::Acquire))
                },
            )
            .await
            .map_err(DownloadError::code)?;
        }
        job.update(|status| status.state = "extracting");
        let tree = staging.join("tree");
        let specification = staging.join("import.json");
        let release = crate::runtime_packages::ReleaseSource {
            engine: build.engine.name().into(),
            backend: build.backend.into(),
            gpu_target: build.gpu_target.map(str::to_owned),
            repository: build.repository.into(),
            tag: build.tag.into(),
            asset: build.asset.into(),
            archive_sha256: sha256.to_owned(),
            archive_bytes: bytes,
            digest_source: source.into(),
            runtime: build
                .runtime
                .map(|runtime| crate::runtime_packages::RuntimeSource {
                    name: runtime.name.into(),
                    version: runtime.version.into(),
                    gpu_target: runtime.gpu_target.into(),
                    url: runtime.download_url(&self.engine_releases.runtimes),
                    archive_sha256: runtime.sha256.into(),
                    archive_bytes: runtime.archive_bytes,
                }),
        };
        let manager = Arc::downgrade(self);
        let written = specification.clone();
        let job_state = job.clone();
        tokio::task::spawn_blocking(move || -> Result<(), &'static str> {
            let stop = AtomicBool::new(false);
            if manager
                .upgrade()
                .is_none_or(|manager| manager.shutting_down.load(Ordering::Acquire))
            {
                stop.store(true, Ordering::Release);
            }
            let runtime_archive = build.runtime.map(|_| runtime_path.as_path());
            let described = archive::assemble(&build, &archive_path, runtime_archive, &tree, &stop)
                .map_err(archive_failure)?;
            let _ = std::fs::remove_file(&archive_path);
            let _ = std::fs::remove_file(&runtime_path);
            job_state.update(|status| status.state = "verifying");
            let licenses = match (build.engine, build.backend) {
                (Engine::Kokoros, _) => {
                    vec!["Apache-2.0".into(), "GPL-3.0-or-later".into()]
                }
                (Engine::Moonshine, _) => {
                    vec!["BSD-3-Clause".into(), "MIT".into(), "PSF-2.0".into()]
                }
                (Engine::FastFlowLm, _) => {
                    vec!["LicenseRef-FastFlowLM-Binary".into(), "MIT".into()]
                }
                (_, "hybrid-rocm") => vec![
                    "LicenseRef-AMD-ROCm".into(),
                    "LicenseRef-AMD-XRT".into(),
                    "MIT".into(),
                ],
                (_, "cuda") => vec![
                    "LicenseRef-NVIDIA-CUDA-Redistributable".into(),
                    "MIT".into(),
                ],
                (_, "rocm") => vec!["LicenseRef-AMD-ROCm".into(), "MIT".into()],
                _ => vec!["MIT".to_owned()],
            };
            let request = crate::runtime_packages::ImportRequest {
                schema_version: 1,
                files: described
                    .sizes
                    .iter()
                    .map(|(path, bytes)| crate::runtime_packages::MemberBytes {
                        path: path.clone(),
                        bytes: *bytes,
                    })
                    .collect(),
                bundle: described.bundle,
                provenance: crate::runtime_packages::Provenance {
                    builder_id: "juiced-engine-installer".into(),
                    source_revision: build.tag.into(),
                    build_revision: format!("sha256:{}", release.archive_sha256),
                    declared_licenses: licenses,
                    declared_sbom: None,
                    release: Some(release),
                },
                declared_target: crate::runtime_packages::DeclaredTarget {
                    operating_system: build.os.into(),
                    architecture: build.architecture.into(),
                    abi: "gnu".into(),
                    compute_paths: vec![build.backend.into()],
                },
            };
            let bytes = serde_json::to_vec(&request).map_err(|_| "engine_specification")?;
            std::fs::write(&written, bytes).map_err(|_| "engine_storage")
        })
        .await
        .map_err(|_| "engine_install_interrupted")??;
        job.update(|status| status.state = "installing");
        let reply = self
            .import_runtime(specification, expected_generation)
            .await
            .map_err(|error| error.code)?;
        Ok(reply.value.package)
    }

    pub async fn add_model(self: &Arc<Self>, request: ModelAdd) -> Result<Value, AddModelError> {
        let (artifact, artifact_set, name, format, file_bytes, model_file, companions) =
            if let Some(path) = request.path.as_deref() {
                if request.artifact.is_some() || request.artifact_set.is_some() {
                    return Err(invalid(
                        "model_source_ambiguous",
                        "Provide an artifact, an artifact set or a linked import path, not several.",
                    )
                    .into());
                }
                let linked = self.linked_import(path).await?;
                (
                    None,
                    None,
                    Some(linked.name),
                    linked.format,
                    linked.bytes,
                    linked.path,
                    Vec::new(),
                )
            } else {
                let store = self.artifacts()?;
                let set_id = request.artifact_set.clone();
                let requested_artifact = request.artifact.clone();
                let inspect = store.clone();
                let (artifact, artifact_set, name) =
                    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
                        match (requested_artifact, set_id) {
                            (None, None) => Err(invalid(
                                "artifact_required",
                                "Provide an artifact or an artifact set.",
                            )),
                            (artifact, Some(set)) => {
                                let view = inspect.artifact_set(&set).map_err(ApiError::from)?;
                                let primary = view
                                    .specification
                                    .members
                                    .iter()
                                    .find(|member| member.name == view.specification.primary)
                                    .ok_or_else(|| {
                                        ApiError::from(crate::artifacts::ArtifactError::SetBinding)
                                    })?;
                                if artifact
                                    .as_ref()
                                    .is_some_and(|artifact| *artifact != primary.artifact)
                                {
                                    return Err(ApiError::from(
                                        crate::artifacts::ArtifactError::SetBinding,
                                    ));
                                }
                                Ok((
                                    primary.artifact.clone(),
                                    Some(set),
                                    Some(primary.name.clone()),
                                ))
                            }
                            (Some(artifact), None) => {
                                let name = inspect.source_file_name(&artifact);
                                let set = companion_set(&inspect, &artifact)?;
                                Ok((artifact, set, name))
                            }
                        }
                    })
                    .await
                    .map_err(|_| interrupted())??;
                let held_artifact = artifact.clone();
                let held_set = artifact_set.clone();
                let (format, file_bytes, model_file, companions) =
                    tokio::task::spawn_blocking(move || {
                        let lease = store
                            .hold_binding_with(&held_artifact, held_set.as_deref(), true)
                            .map_err(ApiError::from)?;
                        let path = lease.path().to_owned();
                        let (format, bytes) = models::detect(&path).map_err(format_error)?;
                        let mut total = bytes;
                        let mut companions = Vec::new();
                        for (name, role, path) in lease.companions() {
                            if role == crate::artifacts::ArtifactRole::Voice {
                                models::voices(path).map_err(format_error)?;
                            }
                            let size = std::fs::metadata(path)
                                .map_err(|_| format_error(models::FormatError::Unavailable))?
                                .len();
                            total = total.saturating_add(size);
                            companions.push(models::Companion {
                                name: name.to_owned(),
                                role,
                            });
                        }
                        let format = match format {
                            models::ModelFormat::MoonshineOrt { .. } => {
                                models::ModelFormat::MoonshineOrt {
                                    model_arch: lease
                                        .companions()
                                        .into_iter()
                                        .find(|(name, role, _)| {
                                            *name == models::MOONSHINE_CONFIG
                                                && *role
                                                    == crate::artifacts::ArtifactRole::Configuration
                                        })
                                        .map(|(_, _, path)| models::moonshine_arch(path))
                                        .transpose()
                                        .map_err(format_error)?,
                                }
                            }
                            other => other,
                        };
                        Ok::<_, ApiError>((format, total, path, companions))
                    })
                    .await
                    .map_err(|_| interrupted())??;
                (
                    Some(artifact),
                    artifact_set,
                    name,
                    format,
                    file_bytes,
                    model_file,
                    companions,
                )
            };
        let companion_arguments =
            models::companion_arguments(&format, &companions).map_err(|error| match error {
                models::CompanionError::Required(code) => ApiError::new(
                    StatusCode::CONFLICT,
                    code,
                    "This model needs companion files. Publish an artifact set with its voices, VAE or text-encoder members and add that set.",
                ),
                models::CompanionError::Unsupported => {
                    ApiError::from(crate::artifacts::ArtifactError::SetUnsupported)
                }
            })?;
        let id = match request.id {
            Some(id) => id,
            None => models::native_default_id(&format)
                .or_else(|| name.as_deref().and_then(models::default_id))
                .ok_or_else(|| {
                    invalid(
                        "model_id_required",
                        "No file name is recorded for this artifact. Provide an explicit id.",
                    )
                })?,
        };
        if id.trim().is_empty()
            || id.trim() != id
            || id.len() > 256
            || id.contains(',')
            || id.chars().any(char::is_control)
        {
            return Err(invalid(
                "invalid_model_id",
                "Model IDs must be nonempty literal text of at most 256 bytes without commas or surrounding whitespace.",
            )
            .into());
        }
        let engine = format.engine();
        let (snapshot, inventory) = self.host().await?;
        let packages = self.package_inventory().await?;
        let installed = crate::engines::installed_packages(&packages);
        let choice = crate::engines::choose(
            engine,
            &inventory,
            &snapshot.report,
            &installed,
            request.backend.as_deref(),
        )
        .map_err(|error| match error {
            ChoiceError::NoUsableBackend => AddModelError::Api(ApiError::new(
                StatusCode::CONFLICT,
                "no_usable_backend",
                "Hardware detection found no available core backend for this engine.",
            )),
            ChoiceError::NotSelectable(_) => AddModelError::Api(ApiError::new(
                StatusCode::CONFLICT,
                "backend_not_selectable",
                "Automatic registration accepts only available core backends; experimental, hybrid and system backends require an explicit registration.",
            )),
            ChoiceError::NotInstalled {
                backend,
                gpu_target,
                installable,
            } => AddModelError::NotInstalled(json!({
                "error": {
                    "code": "engine_not_installed",
                    "message": format!("Install the {} {backend} engine first with POST /juiced/engines/install.", engine.name()),
                    "type": "juiced_error",
                },
                "install": {
                    "engine": engine.name(), "backend": backend, "gpu_target": gpu_target,
                    "installable": installable, "expected_generation": packages["generation"],
                },
            })),
        })?;
        let (estimate, llama) = match &format {
            models::ModelFormat::Gguf { declared, shape }
                if crate::engines::memory::llama(
                    shape,
                    declared.architecture.as_deref(),
                    file_bytes,
                    0,
                    &crate::engines::memory::Plan::new(1, 1, choice.gpu),
                )
                .is_some() =>
            {
                let defaults = crate::engines::memory::LlamaDefaults::for_model(
                    shape,
                    declared.architecture.as_deref(),
                    1,
                );
                let plan = |context| {
                    let mut plan = crate::engines::memory::Plan::new(context, 1, choice.gpu);
                    plan.mtp = defaults.mtp;
                    plan
                };
                let share = self.auto_fit_share(&id);
                let context = crate::engines::memory::auto_context(
                    declared.training_context_tokens,
                    share,
                    |context| {
                        models::estimate_with(&format, file_bytes, 0, Some(&plan(context)))
                            .breakdown
                            .map(|breakdown| breakdown.total())
                    },
                );
                (
                    models::estimate_with(&format, file_bytes, 0, Some(&plan(context))),
                    Some(defaults),
                )
            }
            _ => (
                models::estimate(&format, file_bytes, models::context_tokens(&format)),
                None,
            ),
        };
        let cpu_threads = snapshot
            .report
            .devices
            .cpu
            .as_ref()
            .and_then(|cpu| cpu.physical_cores.or(cpu.usable_threads));
        let registration = models::registration(&models::ProfileInputs {
            id: &id,
            model_file: &model_file,
            artifact: artifact.as_deref(),
            artifact_set: artifact_set.as_deref(),
            package: &choice.package,
            backend: &choice.backend,
            gpu: choice.gpu,
            cpu_threads,
            format: &format,
            estimate: &estimate,
            companion_arguments: &companion_arguments,
            llama: llama.as_ref(),
        });
        let crate::config::Registration(spec) =
            serde_json::from_value(registration.clone()).map_err(|_| {
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
            "engine": engine.name(),
            "backend": choice.backend,
            "package": choice.package,
            "reason": choice.reason,
            "format": format,
            "artifact_set": artifact_set,
            "companions": companions.iter().map(|companion| json!({"name": companion.name, "role": companion.role})).collect::<Vec<_>>(),
            "execution_profile": registration["execution_profile"],
            "estimate": estimate,
            "compatibility": "unqualified",
        }))
    }
}

pub(super) fn format_error(error: models::FormatError) -> ApiError {
    match error {
        models::FormatError::Unavailable => ApiError::new(
            StatusCode::CONFLICT,
            "artifact_unavailable",
            "The artifact file could not be inspected.",
        ),
        models::FormatError::Unsupported => ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_model_format",
            "Only GGUF (llama.cpp, sd.cpp, parakeet.cpp or FUTO asr4all), whisper.cpp ggml, diffusion safetensors, Kokoro ONNX and Moonshine ORT weights can be added automatically.",
        ),
        models::FormatError::Invalid(code) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "The model metadata is invalid or exceeds inspection limits.",
        ),
    }
}

/// A primary that anchors exactly one complete companion set adds that set; split shards
/// and ambiguous anchors stay explicit.
fn companion_set(
    store: &crate::artifacts::ArtifactStore,
    artifact: &str,
) -> Result<Option<String>, ApiError> {
    let digests = std::collections::BTreeSet::from([artifact.to_owned()]);
    let presence = store.presence(&digests).map_err(ApiError::from)?;
    let mut found = None;
    for id in presence.sets.get(artifact).into_iter().flatten() {
        let Ok(view) = store.artifact_set(id) else {
            continue;
        };
        let specification = &view.specification;
        let anchored = specification
            .members
            .iter()
            .any(|member| member.name == specification.primary && member.artifact == artifact);
        if anchored
            && view.members_present
            && specification.members.len() > 1
            && specification
                .members
                .iter()
                .all(|member| member.role != crate::artifacts::ArtifactRole::WeightShard)
        {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(id.clone());
        }
    }
    Ok(found)
}

fn not_published() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "engine_not_published",
        "This engine has no published release build. Import a locally built runtime package with POST /juiced/runtime-packages/import.",
    )
}

fn interrupted() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "model_add_interrupted",
        "Automatic registration was interrupted. Inspect the catalog before retrying.",
    )
}
