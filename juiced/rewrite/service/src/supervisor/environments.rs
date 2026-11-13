use super::*;
use crate::engines::{
    Installed,
    catalog::{self, Build, Engine},
    download::{self, DownloadError},
    environments::{self, Record},
};
use serde_json::{Value, json};
use std::path::Path;

impl Manager {
    pub(crate) fn environment_store(&self) -> PathBuf {
        environments::store(self.config_path.parent().unwrap_or(Path::new(".")))
    }

    /// The compile cache of one environment, outside its read-only root.
    pub(crate) fn environment_cache(&self, id: &str) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(environments::CACHE)
            .join(id)
    }

    pub(super) async fn environment_records(&self) -> Vec<Record> {
        let store = self.environment_store();
        tokio::task::spawn_blocking(move || environments::list(&store))
            .await
            .unwrap_or_default()
    }

    /// The engine inventory rows for installed environments. An environment installed from
    /// a pin the catalog no longer carries keeps no tag, so it is never reported current.
    pub(super) fn environment_packages(records: &[Record]) -> Vec<Installed> {
        records
            .iter()
            .map(|record| {
                let current = catalog::builds().any(|build| {
                    build.engine == record.engine
                        && build.backend == record.backend
                        && build.gpu_target == record.gpu_target.as_deref()
                        && build
                            .environment
                            .is_some_and(|environment| environment.pin() == record.pin)
                });
                Installed {
                    package: record.id.clone(),
                    engine: Some(record.engine),
                    backend: Some(record.backend.clone()),
                    gpu_target: record.gpu_target.clone(),
                    tag: current.then(|| record.tag.clone()),
                    installed: true,
                    declared_host_match: true,
                }
            })
            .collect()
    }

    pub(super) fn environment_views(&self, records: &[Record], models: &[ModelSpec]) -> Vec<Value> {
        let store = self.environment_store();
        records
            .iter()
            .map(|record| {
                let interpreter = environments::interpreter(&store, record);
                let users: Vec<Value> = models
                    .iter()
                    .filter(|model| Path::new(&model.execution_profile.command) == interpreter)
                    .map(|model| json!({"model": model.id, "selection": "current"}))
                    .collect();
                json!({
                    "package": record.id,
                    "kind": "environment",
                    "phase": "installed",
                    "engine": record.engine,
                    "release": {
                        "engine": record.engine, "backend": record.backend,
                        "gpu_target": record.gpu_target, "repository": record.repository,
                        "tag": record.tag, "version": record.version,
                        "digest_source": "pinned",
                    },
                    "declared_host_match": true,
                    "reserved_bytes": record.bytes,
                    "files": record.files,
                    "content_sha256": record.content_sha256,
                    "binding_leases": users.len(),
                    "models": users,
                })
            })
            .collect()
    }

    /// Refuses an environment install that cannot fit, before anything downloads.
    pub(super) fn environment_space(&self, build: &Build) -> Result<(), ApiError> {
        let Some(environment) = build.environment else {
            return Ok(());
        };
        let downloads: u64 = match &environment.source {
            catalog::EnvironmentSource::Parts { parts } => {
                parts.iter().map(|part| part.bytes).sum()
            }
            catalog::EnvironmentSource::Uv { .. } => environment.installed_bytes / 2,
        };
        let needed = environment.installed_bytes.saturating_add(downloads);
        let parent = self.config_path.parent().unwrap_or(Path::new("."));
        match environments::available_bytes(parent) {
            Some(free) if free < needed => Err(ApiError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "engine_storage_space",
                format!(
                    "{} needs about {} GiB free for its download and installation; {} GiB are free.",
                    environment.version,
                    needed.div_ceil(1 << 30),
                    free / (1 << 30)
                ),
            )),
            _ => Ok(()),
        }
    }

    /// Downloads every pinned archive of an environment build and installs it.
    pub(super) async fn install_environment(
        self: &Arc<Self>,
        job: &Arc<super::engines::EngineJob>,
        policy: crate::artifacts::SourcePolicy,
        staging: &Path,
    ) -> Result<String, &'static str> {
        let build = job.build;
        let environment = build.environment.ok_or("engine_environment_layout")?;
        let downloads: Vec<(String, &'static str, u64, PathBuf)> = match &environment.source {
            catalog::EnvironmentSource::Parts { parts } => parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    (
                        format!(
                            "{}/{}/releases/download/{}/{}",
                            self.engine_releases.downloads.trim_end_matches('/'),
                            build.repository,
                            build.tag,
                            part.asset
                        ),
                        part.sha256,
                        part.bytes,
                        staging.join(format!("part-{index}")),
                    )
                })
                .collect(),
            catalog::EnvironmentSource::Uv { .. } => vec![(
                build.download_url(&self.engine_releases.downloads),
                build.sha256.ok_or("engine_digest_unavailable")?,
                build.archive_bytes,
                staging.join("archive"),
            )],
        };
        job.update(|status| {
            status.state = "downloading";
            status.digest_source = Some("pinned");
            status.archive_sha256 = build.sha256.map(str::to_owned);
            status.total_bytes = Some(downloads.iter().map(|download| download.2).sum());
        });
        let mut archives = Vec::new();
        for (index, (url, sha256, bytes, target)) in downloads.into_iter().enumerate() {
            let manager = Arc::downgrade(self);
            download::fetch(
                &url,
                policy,
                sha256,
                bytes,
                &target,
                if index == 0 {
                    &job.downloaded
                } else {
                    &job.runtime_downloaded
                },
                move || {
                    manager
                        .upgrade()
                        .is_none_or(|manager| manager.shutting_down.load(Ordering::Acquire))
                },
            )
            .await
            .map_err(DownloadError::code)?;
            archives.push(target);
        }
        let fetched =
            job.downloaded.load(Ordering::Acquire) + job.runtime_downloaded.load(Ordering::Acquire);
        job.downloaded.store(fetched, Ordering::Release);
        job.runtime_downloaded.store(0, Ordering::Release);
        job.update(|status| {
            status.state = "installing";
            status.total_bytes = Some(fetched + environment.installed_bytes);
        });
        let progress = job.clone();
        let manager = Arc::downgrade(self);
        let store = self.environment_store();
        let staging = staging.to_owned();
        let logs = self.logs().clone();
        tokio::task::spawn_blocking(move || {
            let cancelled = AtomicBool::new(false);
            let watcher = || {
                manager
                    .upgrade()
                    .is_none_or(|manager| manager.shutting_down.load(Ordering::Acquire))
            };
            let done = AtomicBool::new(false);
            // Installation writes far more than it downloads, so its progress is the
            // space the staging tree has taken, bounded by the declared installed size.
            let start = environments::available_bytes(&staging);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    while !done.load(Ordering::Acquire) {
                        if let (Some(start), Some(now)) =
                            (start, environments::available_bytes(&staging))
                        {
                            progress.runtime_downloaded.store(
                                start.saturating_sub(now).min(environment.installed_bytes),
                                Ordering::Release,
                            );
                        }
                        if watcher() {
                            cancelled.store(true, Ordering::Release);
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                });
                let log = |message: String| logs.service(crate::logs::Level::Info, &message);
                let result = environments::install(environments::InstallRequest {
                    build,
                    environment,
                    staging: &staging,
                    store: &store,
                    archives,
                    cancelled: &cancelled,
                    log: &log,
                });
                done.store(true, Ordering::Release);
                result
            })
        })
        .await
        .map_err(|_| "engine_install_interrupted")?
        .map(|record| {
            self.logs().service(
                crate::logs::Level::Info,
                &format!(
                    "Installed engine environment {} ({}, {} files, {} bytes)",
                    record.id, record.version, record.files, record.bytes
                ),
            );
            record.id
        })
        .map_err(|error| error.code())
    }

    /// Removes the unused environments of one engine backend; `None` when it has none.
    pub(super) async fn uninstall_environments(
        &self,
        engine: Engine,
        backend: &str,
    ) -> Option<Result<Value, ApiError>> {
        let records: Vec<Record> = self
            .environment_records()
            .await
            .into_iter()
            .filter(|record| record.engine == engine && record.backend == backend)
            .collect();
        if records.is_empty() {
            return None;
        }
        let store = self.environment_store();
        let interpreters: Vec<String> = records
            .iter()
            .map(|record| {
                environments::interpreter(&store, record)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let users: Vec<String> = self
            .config
            .lock()
            .await
            .models
            .iter()
            .filter(|model| interpreters.contains(&model.execution_profile.command))
            .map(|model| model.id.clone())
            .collect();
        if !users.is_empty() {
            return Some(Err(ApiError::new(
                StatusCode::CONFLICT,
                "engine_in_use",
                format!(
                    "Delete or re-point the models that use this backend first: {}.",
                    users.join(", ")
                ),
            )));
        }
        let caches: Vec<PathBuf> = records
            .iter()
            .map(|record| self.environment_cache(&record.id))
            .collect();
        let removed = tokio::task::spawn_blocking(move || {
            let mut freed = 0u64;
            let mut removed = Vec::new();
            for (record, cache) in records.iter().zip(caches) {
                let entry = environments::entry(&store, &record.id).ok_or(())?;
                environments::remove(&entry).map_err(|_| ())?;
                let _ = std::fs::remove_dir_all(cache);
                freed += record.bytes;
                removed.push(record.id.clone());
            }
            Ok::<_, ()>((removed, freed))
        })
        .await;
        Some(match removed {
            Ok(Ok((removed, freed))) => Ok(json!({
                "engine": engine.name(), "backend": backend,
                "removed_packages": removed, "freed_bytes": freed,
            })),
            _ => Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "engine_uninstall_failed",
                "Engine environment removal stopped. Retry to finish.",
            )),
        })
    }

    /// Points an automatic vLLM registration at its environment's interpreter and sizes
    /// `--gpu-memory-utilization` to the estimate on the device vLLM will use.
    pub(super) async fn environment_registration(
        &self,
        registration: &mut Value,
        choice: &crate::engines::Choice,
        report: &crate::hardware::HostReport,
    ) -> Result<(), ApiError> {
        let store = self.environment_store();
        let record = self
            .environment_records()
            .await
            .into_iter()
            .find(|record| record.id == choice.package)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "engine_not_installed",
                    "The selected engine environment is no longer installed.",
                )
            })?;
        let vendor = match choice.backend.as_str() {
            "cuda" => crate::hardware::GpuVendor::Nvidia,
            _ => crate::hardware::GpuVendor::Amd,
        };
        let device_bytes = report
            .devices
            .gpus
            .iter()
            .filter(|gpu| gpu.vendor == vendor)
            .filter_map(|gpu| {
                let vram = gpu.vram_total_bytes.unwrap_or_default();
                Some(if gpu.integrated == Some(true) {
                    vram.max(gpu.gtt_total_bytes.unwrap_or_default())
                } else {
                    vram
                })
                .filter(|bytes| *bytes > 0)
            })
            .max();
        let profile = &mut registration["execution_profile"];
        if let Some(profile) = profile.as_object_mut() {
            profile.remove("runtime_package");
            profile.insert(
                "command".into(),
                json!(environments::interpreter(&store, &record)),
            );
        }
        let resident = registration["resident_bytes"].as_u64().unwrap_or_default();
        if let (Some(total), Some(args)) = (
            device_bytes,
            registration["execution_profile"]["args"].as_array_mut(),
        ) {
            let fraction = (resident as f64 / total as f64 + 0.02).clamp(0.05, 0.92);
            args.extend([
                json!("--gpu-memory-utilization"),
                json!(format!("{:.2}", (fraction * 100.0).ceil() / 100.0)),
            ]);
        }
        Ok(())
    }
}
