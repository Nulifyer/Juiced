//! First-boot adoption of a native Juiced configuration directory.
//!
//! When `JUICED_NATIVE_CONFIG_DIR` names a native configuration directory and the rewrite
//! configuration carries no adoption marker, the service previews the native import, installs
//! the engines its enabled models need, registers every importable model with its native ID,
//! policy, options and in-place weights and companions, adds native aliases, and records a
//! marker. The native directory is only read.

use super::*;
use crate::{
    config::NativeAdoption,
    supervisor::{NativeImportModel, engines::EngineInstall},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const SOURCE_ENV: &str = "JUICED_NATIVE_CONFIG_DIR";
pub const ROOTS_ENV: &str = "JUICED_NATIVE_IMPORT_ROOTS";
const MAX_ROUNDS: usize = 4;
const INSTALL_POLL: Duration = Duration::from_secs(2);
const INSTALL_WAIT: Duration = Duration::from_secs(3 * 60 * 60);

#[derive(Default)]
pub struct Status {
    view: std::sync::Mutex<Value>,
    target: std::sync::Mutex<Option<(PathBuf, Vec<PathBuf>)>>,
    running: AtomicBool,
}

struct Running<'a>(&'a AtomicBool);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Status {
    fn set(&self, value: Value) {
        *self.view.lock().unwrap_or_else(|error| error.into_inner()) = value;
    }
    fn update(&self, change: impl FnOnce(&mut serde_json::Map<String, Value>)) {
        let mut status = self.view.lock().unwrap_or_else(|error| error.into_inner());
        if !status.is_object() {
            *status = json!({});
        }
        change(status.as_object_mut().expect("adoption status object"));
    }
    fn view(&self) -> Value {
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    fn begin(&self) -> Option<Running<'_>> {
        self.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Running(&self.running))
    }
}

/// Reads the adoption source and extra read roots from the environment. Extra roots default to
/// the native configuration directory's parent, which holds its Hugging Face cache.
pub fn from_env() -> Option<(PathBuf, Vec<PathBuf>)> {
    let source = std::env::var_os(SOURCE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())?;
    let roots = match std::env::var_os(ROOTS_ENV).filter(|value| !value.is_empty()) {
        Some(value) => std::env::split_paths(&value)
            .filter(|path| path.is_absolute() && path.parent().is_some())
            .take(crate::config::MAXIMUM_IMPORT_ROOTS)
            .collect(),
        None => source
            .parent()
            .filter(|parent| parent.parent().is_some())
            .map(|parent| vec![parent.to_owned()])
            .unwrap_or_default(),
    };
    Some((source, roots))
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn policy_of(row: &Value) -> Policy {
    serde_json::from_value(row["adopted_policy"].clone()).unwrap_or(Policy::Disabled)
}

impl Manager {
    pub fn native_adoption_status(&self) -> Value {
        self.native_adoption.view()
    }

    /// Starts adoption in the background; serving continues while it runs.
    pub fn start_native_adoption(
        self: &Arc<Self>,
        source: PathBuf,
        extra_roots: Vec<PathBuf>,
    ) -> tokio::task::JoinHandle<()> {
        self.native_adoption.set(json!({
            "state": "starting", "source": source, "extra_roots": extra_roots,
        }));
        *self
            .native_adoption
            .target
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some((source.clone(), extra_roots.clone()));
        let manager = self.clone();
        tokio::spawn(async move {
            manager.run_adoption(source, extra_roots).await;
        })
    }

    /// Registers models still waiting for an engine once an installation completes.
    pub(super) fn resume_native_adoption(self: &Arc<Self>) {
        let Some((source, extra_roots)) = self
            .native_adoption
            .target
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        else {
            return;
        };
        let manager = self.clone();
        tokio::spawn(async move {
            let pending = manager
                .config
                .lock()
                .await
                .native_adoption
                .as_ref()
                .is_some_and(|marker| !marker.pending.is_empty());
            if pending {
                manager.run_adoption(source, extra_roots).await;
            }
        });
    }

    async fn run_adoption(self: &Arc<Self>, source: PathBuf, extra_roots: Vec<PathBuf>) {
        let Some(_running) = self.native_adoption.begin() else {
            return;
        };
        {
            let manager = self;
            let outcome = manager.adopt_native(source, extra_roots).await;
            if let Err(error) = outcome {
                manager.logs().service(
                    crate::logs::Level::Warning,
                    &format!(
                        "Native adoption stopped: {} ({}). It retries on the next start.",
                        error.message, error.code
                    ),
                );
                manager.native_adoption.update(|status| {
                    status.insert("state".into(), json!("failed"));
                    status.insert(
                        "error".into(),
                        json!({"code": error.code, "message": error.message}),
                    );
                });
            }
        }
    }

    async fn adopt_native(
        self: &Arc<Self>,
        source: PathBuf,
        extra_roots: Vec<PathBuf>,
    ) -> Result<(), ApiError> {
        let log = |level, message: String| self.logs().service(level, &message);
        let marker = self.config.lock().await.native_adoption.clone();
        let only: Option<BTreeSet<String>> = match &marker {
            Some(marker) if marker.pending.is_empty() => {
                self.native_adoption.update(|status| {
                    status.insert("state".into(), json!("adopted"));
                    status.insert("marker".into(), json!(marker));
                });
                return Ok(());
            }
            Some(marker) => Some(marker.pending.iter().cloned().collect()),
            None => None,
        };
        log(
            crate::logs::Level::Info,
            match &only {
                None => format!("Adopting native configuration from {}", source.display()),
                Some(pending) => format!(
                    "Resuming native adoption for {} pending models from {}",
                    pending.len(),
                    source.display()
                ),
            },
        );
        let mut installed = BTreeSet::new();
        let mut failed_installs = BTreeSet::new();
        let mut applied = Vec::new();
        let mut preview = Value::Null;
        for round in 0..MAX_ROUNDS {
            self.native_adoption.update(|status| {
                status.insert("state".into(), json!("inspecting"));
                status.insert("round".into(), json!(round + 1));
            });
            preview = self
                .native_import_preview(source.clone(), None, &extra_roots)
                .await?;
            let rows: Vec<Value> = preview["models"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|row| {
                    only.as_ref().is_none_or(|only| {
                        row["native_id"]
                            .as_str()
                            .is_some_and(|id| only.contains(id))
                    })
                })
                .collect();
            let mut installs: Vec<(String, String)> = Vec::new();
            for row in &rows {
                if row["status"] == "engine_install_needed" && policy_of(row) != Policy::Disabled {
                    let install = &row["engine"]["install"];
                    let key = (
                        install["engine"].as_str().unwrap_or_default().to_owned(),
                        install["backend"].as_str().unwrap_or_default().to_owned(),
                    );
                    if !key.0.is_empty()
                        && !installs.contains(&key)
                        && !installed.contains(&key)
                        && !failed_installs.contains(&key)
                    {
                        installs.push(key);
                    }
                }
            }
            let ready: Vec<NativeImportModel> = rows
                .iter()
                .filter(|row| {
                    row["status"] == "ready"
                        && row["conflicts"].as_array().is_none_or(Vec::is_empty)
                })
                .filter_map(|row| {
                    Some(NativeImportModel {
                        native_id: row["native_id"].as_str()?.to_owned(),
                        id: None,
                        policy: Some(policy_of(row)),
                        include: true,
                    })
                })
                .collect();
            if !ready.is_empty() {
                self.native_adoption.update(|status| {
                    status.insert("state".into(), json!("registering"));
                });
                let generation = self.config.lock().await.generation;
                let count = ready.len();
                match self
                    .native_import_apply(
                        source.clone(),
                        None,
                        ready,
                        true,
                        generation,
                        &extra_roots,
                    )
                    .await
                {
                    Ok(result) => {
                        log(
                            crate::logs::Level::Info,
                            format!("Native adoption registered {count} models"),
                        );
                        applied.push(result);
                    }
                    Err(error) if error.code == "revision_conflict" => continue,
                    Err(error) => return Err(error),
                }
            }
            if installs.is_empty() {
                break;
            }
            for (engine, backend) in installs {
                self.native_adoption.update(|status| {
                    status.insert("state".into(), json!("installing_engine"));
                    status.insert(
                        "installing".into(),
                        json!({"engine": engine, "backend": backend}),
                    );
                });
                log(
                    crate::logs::Level::Info,
                    format!("Native adoption is installing {engine} {backend}"),
                );
                match self.adoption_install(&engine, &backend).await {
                    Ok(()) => {
                        log(
                            crate::logs::Level::Info,
                            format!("Native adoption installed {engine} {backend}"),
                        );
                        installed.insert((engine, backend));
                    }
                    Err(error) => {
                        log(
                            crate::logs::Level::Warning,
                            format!(
                                "Native adoption could not install {engine} {backend}: {} ({})",
                                error.message, error.code
                            ),
                        );
                        failed_installs.insert((engine, backend));
                    }
                }
            }
        }
        let rows = preview["models"].as_array().cloned().unwrap_or_default();
        let registered: BTreeSet<String> = applied
            .iter()
            .flat_map(|result| result["models"].as_array().cloned().unwrap_or_default())
            .filter(|row| row["status"] == "imported" || row["status"] == "already_imported")
            .filter_map(|row| row["native_id"].as_str().map(str::to_owned))
            .collect();
        let config = self.config.lock().await.clone();
        let mut pending = Vec::new();
        let mut models = Vec::new();
        for row in &rows {
            let Some(native_id) = row["native_id"].as_str() else {
                continue;
            };
            if only.as_ref().is_some_and(|only| !only.contains(native_id)) {
                continue;
            }
            let id = row["proposed_id"].as_str().unwrap_or(native_id);
            let present = config.models.iter().find(|model| model.id == id);
            let outcome = match (present, row["status"].as_str()) {
                (Some(_), _) if registered.contains(native_id) => "registered",
                (Some(_), Some("already_imported")) => "registered",
                (_, Some("engine_install_needed")) => {
                    pending.push(native_id.to_owned());
                    if policy_of(row) == Policy::Disabled {
                        "waiting_for_engine"
                    } else {
                        "engine_install_failed"
                    }
                }
                (Some(_), _) => "conflict",
                (None, Some("blocked")) => "blocked",
                (None, _) => "not_registered",
            };
            models.push(json!({
                "native_id": native_id, "id": id, "outcome": outcome,
                "policy": present.map(|model| model.policy),
                "adopted_policy": row["adopted_policy"], "recipe": row["recipe"],
                "engine": row["engine"]["engine"], "backend": row["engine"]["backend"],
                "weights": row["weights"], "companions": row["companions"],
                "blockers": row["blockers"],
            }));
        }
        if let Some(only) = &only {
            for native_id in only {
                if !rows
                    .iter()
                    .any(|row| row["native_id"] == native_id.as_str())
                {
                    pending.push(native_id.clone());
                }
            }
        }
        pending.sort();
        pending.dedup();
        let marker = NativeAdoption {
            source: source.clone(),
            adopted_unix_ms: marker
                .as_ref()
                .map_or_else(unix_ms, |marker| marker.adopted_unix_ms),
            pending: pending.clone(),
        };
        self.record_adoption(marker.clone()).await?;
        if !applied.is_empty() {
            let manager = self.clone();
            tokio::spawn(async move { manager.start_hot().await });
        }
        let counts = models
            .iter()
            .fold(BTreeMap::<String, u64>::new(), |mut counts, row| {
                *counts
                    .entry(row["outcome"].as_str().unwrap_or_default().to_owned())
                    .or_default() += 1;
                counts
            });
        log(
            crate::logs::Level::Info,
            format!(
                "Native adoption finished: {} ({} pending)",
                counts
                    .iter()
                    .map(|(outcome, count)| format!("{count} {outcome}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                pending.len()
            ),
        );
        self.native_adoption.set(json!({
            "state": "adopted", "source": source, "extra_roots": extra_roots,
            "marker": marker, "counts": counts, "models": models,
            "aliases": applied.iter().flat_map(|result| result["aliases"].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>(),
            "global_settings": preview["global_settings"], "documents": preview["documents"],
            "source_modified": false, "weights_copied": false,
        }));
        Ok(())
    }

    async fn record_adoption(self: &Arc<Self>, marker: NativeAdoption) -> Result<(), ApiError> {
        let config = self.config.clone().lock_owned().await;
        if config.native_adoption.as_ref() == Some(&marker) {
            return Ok(());
        }
        let mut next = config.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration generation cannot advance.",
            )
        })?;
        next.native_adoption = Some(marker);
        let next = Self::check_config_size(next).await?;
        self.publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        Ok(())
    }

    async fn adoption_install(
        self: &Arc<Self>,
        engine: &str,
        backend: &str,
    ) -> Result<(), ApiError> {
        let deadline = Instant::now() + INSTALL_WAIT;
        let job = loop {
            let generation = self.package_inventory().await?["generation"]
                .as_u64()
                .unwrap_or_default();
            match self
                .install_engine(EngineInstall {
                    engine: engine.to_owned(),
                    backend: backend.to_owned(),
                    expected_generation: generation,
                })
                .await
            {
                Ok((_, reply)) if reply["already_installed"] == true => return Ok(()),
                Ok((_, reply)) => break reply["job"]["id"].as_str().unwrap_or_default().to_owned(),
                Err(error)
                    if matches!(
                        error.code,
                        "engine_install_busy" | "manifest_revision_conflict"
                    ) && Instant::now() < deadline =>
                {
                    tokio::time::sleep(INSTALL_POLL).await;
                }
                Err(error) => return Err(error),
            }
        };
        loop {
            let view = self
                .engine_jobs
                .views()
                .into_iter()
                .find(|view| view["id"] == job.as_str());
            match view.as_ref().and_then(|view| view["state"].as_str()) {
                Some("complete") => return Ok(()),
                Some("failed") => {
                    return Err(ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "engine_install_failed",
                        format!(
                            "The engine installation failed: {}.",
                            view.as_ref()
                                .and_then(|view| view["error"].as_str())
                                .unwrap_or("unknown error")
                        ),
                    ));
                }
                None => {
                    return Err(ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "engine_job_lost",
                        "The engine installation job is no longer tracked.",
                    ));
                }
                _ if Instant::now() >= deadline => {
                    return Err(ApiError::new(
                        StatusCode::GATEWAY_TIMEOUT,
                        "engine_install_timeout",
                        "The engine installation did not finish in time.",
                    ));
                }
                _ => tokio::time::sleep(INSTALL_POLL).await,
            }
        }
    }
}
