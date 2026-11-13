use crate::{
    ApiError,
    config::{Config, ConfigurationBinding, ModelSpec, Operation, Policy},
};
use crate::{
    auth::RequestContext,
    cache::{CacheError, CacheStore, SnapshotIdentity},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard, watch};

mod access;
mod aliases;
mod barista;
pub use aliases::AliasChange;
mod images;
mod options;
pub use options::{OptionsApply, OptionsError};
mod artifact_inventory;
mod artifact_revision;
mod runtime_selection;
pub use runtime_selection::{RuntimeChange, RuntimeReply};
mod cache_reply;
mod discovery;
pub use artifact_revision::ArtifactRevisionChange;
pub(crate) use discovery::{Discovered, loaded_entry, model_type, revision as catalog_revision};
mod engines;
pub use engines::{AddModelError, EngineInstall, ModelAdd};
mod intake;
pub use intake::{ImportListQuery, ImportPathRequest, UploadQuery};
mod local_library;
pub use local_library::LocalDelete;
pub use storage::Reclaim;
mod fit;
pub use fit::FitQuery;
mod flm;
pub use flm::{FlmModelAdd, FlmPull};
mod host;
mod llama;
pub use llama::Target as LlamaTarget;
mod native;
mod native_adoption;
pub use native_adoption::{
    ROOTS_ENV as NATIVE_ADOPTION_ROOTS_ENV, SOURCE_ENV as NATIVE_ADOPTION_SOURCE_ENV,
    from_env as native_adoption_from_env,
};
mod native_import;
mod native_management;
pub use native_import::{NativeImportModel, NativeImportRequest};
pub use native_management::{NativeModel, ReleasedWeights};
mod physical;
mod publication;
mod quant_sync;
pub use quant_sync::VariantSync;
mod residency;
pub use residency::{KeepAlive, ResidencyChange, ResidencyView, UnloadReason, parse_keep_alive};
mod sessions;
mod state;
mod storage;
mod voices;
mod wyoming;
pub use cache_reply::CacheReply;
pub use sessions::{SessionAction, SessionRecord};

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

struct Budget {
    limit: u64,
    used: AtomicU64,
}

struct Reservation {
    budget: Option<Arc<Budget>>,
    bytes: u64,
}

impl Budget {
    fn after_reservation(&self, used: u64, bytes: u64) -> Option<u64> {
        used.checked_add(bytes).filter(|next| *next <= self.limit)
    }

    fn reserve(self: &Arc<Self>, bytes: u64) -> Result<Reservation, ApiError> {
        if bytes == 0 {
            return Ok(Reservation {
                budget: None,
                bytes: 0,
            });
        }
        self.used.try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            self.after_reservation(used, bytes)
        }).map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "resident_budget", "The declared resident budget is full. Unload an idle On demand model or increase the configured budget."))?;
        Ok(Reservation {
            budget: Some(self.clone()),
            bytes,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

struct StartupResources {
    _reservation: Reservation,
    host: Mutex<Option<crate::admission::MemoryReservation>>,
    claims: Mutex<StartupClaims>,
}

#[derive(Default)]
struct StartupClaims {
    power: Option<crate::power::Activity>,
    _permit: Option<crate::startup::StartupPermit>,
    _preparation: Option<Arc<PreparationFlag>>,
}

impl StartupResources {
    fn release_startup(&self) {
        let claims = std::mem::take(
            &mut *self
                .claims
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        drop(claims);
    }
}

#[derive(Clone)]
struct LoadFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl From<&ApiError> for LoadFailure {
    fn from(error: &ApiError) -> Self {
        Self {
            status: error.status,
            code: error.code,
            message: error.message.clone(),
        }
    }
}

struct PreparationFlag {
    flag: Arc<AtomicBool>,
    queued: Arc<AtomicBool>,
    completed: watch::Sender<Option<Result<(), LoadFailure>>>,
    deadline: tokio::time::Instant,
}

impl PreparationFlag {
    fn begin(worker: &Worker, deadline: tokio::time::Instant) -> Result<Arc<Self>, ApiError> {
        if worker
            .preparing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(preparation_pending());
        }
        worker.startup_waiting.store(true, Ordering::Release);
        let (completed, _) = watch::channel(None);
        let flag = Arc::new(Self {
            flag: worker.preparing.clone(),
            queued: worker.startup_waiting.clone(),
            completed,
            deadline,
        });
        *worker
            .attempt
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Arc::downgrade(&flag);
        Ok(flag)
    }

    fn finish(&self, result: Result<(), LoadFailure>) {
        self.completed.send_replace(Some(result));
    }
}

impl Drop for PreparationFlag {
    fn drop(&mut self) {
        self.queued.store(false, Ordering::Release);
        self.flag.store(false, Ordering::Release);
        let unfinished = self.completed.borrow().is_none();
        if unfinished {
            self.finish(Err(LoadFailure {
                status: StatusCode::CONFLICT,
                code: "worker_preparation_cancelled",
                message:
                    "The previous worker startup was cancelled. Retry after its owned work settles."
                        .into(),
            }));
        }
    }
}

struct Preparation {
    resources: Arc<StartupResources>,
    _flag: Arc<PreparationFlag>,
}

impl Preparation {
    fn begin(
        flag: Arc<PreparationFlag>,
        reservation: Reservation,
        permit: crate::startup::StartupPermit,
    ) -> Self {
        flag.queued.store(false, Ordering::Release);
        Self {
            resources: Arc::new(StartupResources {
                _reservation: reservation,
                host: Mutex::new(None),
                claims: Mutex::new(StartupClaims {
                    power: None,
                    _permit: Some(permit),
                    _preparation: Some(flag.clone()),
                }),
            }),
            _flag: flag,
        }
    }
}

fn preparation_pending() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "worker_preparation_pending",
        "The previous worker preparation has not settled. Its resources remain reserved.",
    )
}

fn startup_completion_timeout() -> ApiError {
    ApiError::new(
        StatusCode::GATEWAY_TIMEOUT,
        "startup_completion_timeout",
        "The selected worker startup has not settled within its completion wait.",
    )
}

fn startup_error(error: crate::startup::Error) -> ApiError {
    ApiError::new(
        StatusCode::from_u16(error.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        error.code,
        error.message,
    )
}

fn preparation_identity_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "worker_identity",
        "Model or engine identity is unavailable or changed during inspection.",
    )
}

fn preparation_profile_error(error: crate::profiles::Error) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, error.code, error.message)
}

fn preparation_configuration_error(error: crate::runtime_inputs::Refusal) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        error.code(),
        error.public_message(),
    )
}

fn fatal_runtime_refusal(error: &crate::runtime_identity::Refusal) -> bool {
    cfg!(target_os = "linux")
        || matches!(
            error.code,
            "model_artifact_changed" | "unbound_runtime_config"
        )
}

async fn blocking_preparation<T: Send + 'static, E: Send + 'static>(
    preparation: Preparation,
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
    interrupted: &'static str,
) -> Result<(Result<T, E>, Preparation), ApiError> {
    tokio::task::spawn_blocking(move || {
        let preparation = preparation;
        let result = work();
        (result, preparation)
    })
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            interrupted,
            "Worker preparation was interrupted.",
        )
    })
}

struct WorkerResources {
    plan: Arc<crate::profiles::LaunchPlan>,
    _cache: Option<Arc<CacheStore>>,
    speech: Option<Arc<SpeechClaims>>,
    startup: Arc<StartupResources>,
    uncertain: Option<Arc<physical::UncertainClaims>>,
    state: Option<Arc<state::RetainedStates>>,
    writer_model: Option<Arc<str>>,
    writer_generation: AtomicU64,
}

impl Drop for WorkerResources {
    fn drop(&mut self) {
        // The process owner releases these resources only after positive child exit.
        if let (Some(cache), Some(model)) = (&self._cache, &self.writer_model) {
            let generation = self.writer_generation.load(Ordering::Acquire);
            if generation != 0 {
                cache.acknowledge_writer_exit(model, generation);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    fn relative_configuration(directory: &std::path::Path, id: &str) -> Vec<u8> {
        std::fs::write(directory.join("weights.gguf"), b"owned relative weights").unwrap();
        let config = json!({
            "schema_version":1,"inhibit_suspend":false,"resident_budget_bytes":4096,
            "startup_timeout_ms":1000,"request_timeout_ms":1000,
            "models":[{"id":id,"model_file":"weights.gguf","provider":"llama-cpp",
                "command":"./bin/engine","args":["--model","{model}","--port","{port}"],
                "runtime_bundle":{"schema_version":1,"implementation_id":"llama-cpp","root":"runtime",
                    "entrypoint":"engine","library_directories":[],
                    "files":[{"path":"engine","sha256":"a".repeat(64),"role":"executable"}]},
                "resident_bytes":1024,"max_inflight":1,"operations":["completions"],
                "policy":"on-demand","revision":1}]
        });
        let bytes = serde_json::to_vec_pretty(&config).unwrap();
        std::fs::write(directory.join("config.json"), &bytes).unwrap();
        bytes
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claimed_configuration_alias_retarget_keeps_reads_paths_and_publication_on_the_original_store()
     {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        let foreign = directory.path().join("foreign");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let original = relative_configuration(&store, "owned");
        let untouched = relative_configuration(&foreign, "foreign");
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&store, &alias).unwrap();
        let binding = ConfigurationBinding::claim(alias.join("config.json")).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&foreign, &alias).unwrap();
        let config = binding.read().unwrap();
        assert_eq!(config.schema_version, 2);
        assert_eq!(config.models[0].id, "owned");
        assert_eq!(
            config.models[0].model_file,
            std::path::Path::new("weights.gguf")
        );
        assert_eq!(std::fs::read(store.join("config.json")).unwrap(), original);
        let manager = Manager::assemble(config, binding.path, Some(binding.root), false).unwrap();
        let canonical = std::fs::canonicalize(&store).unwrap();
        assert_eq!(manager.config_path, canonical.join("config.json"));
        let worker = manager.worker("owned").unwrap();
        assert_eq!(worker.spec.model_file, canonical.join("weights.gguf"));
        assert_eq!(
            std::path::Path::new(&worker.spec.execution_profile.command),
            canonical.join("./bin/engine")
        );
        assert_eq!(
            worker.spec.execution_profile.runtime_bundle().unwrap().root,
            canonical.join("runtime")
        );
        assert_eq!(manager.cache_directory, canonical.join("kv-cache"));
        assert!(canonical.join("artifacts").is_dir());
        assert!(!foreign.join("kv-cache").exists());
        assert!(!foreign.join("artifacts").exists());
        manager
            .set_policy("owned", Policy::Disabled, 1)
            .await
            .unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.join("config.json")).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert_eq!(saved["models"][0]["model_file"], "weights.gguf");
        assert_eq!(
            saved["models"][0]["execution_profile"]["command"],
            "./bin/engine"
        );
        assert_eq!(
            saved["models"][0]["execution_profile"]["runtime_bundle"]["root"],
            "runtime"
        );
        assert_eq!(
            std::fs::read(foreign.join("config.json")).unwrap(),
            untouched
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaced_claimed_configuration_root_refuses_activation_and_leaves_both_stores_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let original = relative_configuration(&store, "owned");
        let binding = ConfigurationBinding::claim(store.join("config.json")).unwrap();
        let retired = directory.path().join("retired");
        std::fs::rename(&store, &retired).unwrap();
        std::fs::create_dir(&store).unwrap();
        let untouched = relative_configuration(&store, "replacement");
        assert!(binding.read().is_err());
        assert_eq!(
            std::fs::read(retired.join("config.json")).unwrap(),
            original
        );
        assert_eq!(std::fs::read(store.join("config.json")).unwrap(), untouched);
        assert!(!store.join("kv-cache").exists());
        assert!(!store.join("artifacts").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replaced_live_configuration_root_rejects_model_load_and_configuration_publication() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let original = relative_configuration(&store, "owned");
        let manager = Manager::open(store.join("config.json")).unwrap();
        let retired = directory.path().join("retired");
        std::fs::rename(&store, &retired).unwrap();
        std::fs::create_dir(&store).unwrap();
        let untouched = relative_configuration(&store, "replacement");
        assert_eq!(
            manager.load("owned").await.unwrap_err().code,
            "storage_root_unavailable"
        );
        assert_eq!(
            manager
                .set_policy("owned", Policy::Disabled, 1)
                .await
                .unwrap_err()
                .code,
            "storage_root_unavailable"
        );
        assert!(manager.processes.snapshots().is_empty());
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(manager.startup.snapshot().active, 0);
        assert_eq!(
            std::fs::read(retired.join("config.json")).unwrap(),
            original
        );
        assert_eq!(std::fs::read(store.join("config.json")).unwrap(), untouched);
        assert!(!store.join("kv-cache").exists());
        assert!(!store.join("artifacts").exists());
    }

    #[cfg(unix)]
    #[test]
    fn startup_refuses_a_configuration_file_symlink_and_the_reserved_control_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        let foreign = directory.path().join("foreign");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let untouched = relative_configuration(&foreign, "foreign");
        std::os::unix::fs::symlink(foreign.join("config.json"), store.join("config.json")).unwrap();
        assert!(Manager::open(store.join("config.json")).is_err());
        #[cfg(feature = "test-fixture")]
        {
            let config: Config = serde_json::from_slice(&untouched).unwrap();
            assert!(Manager::new(config, store.join("config.json")).is_err());
        }
        assert!(ConfigurationBinding::claim(store.join(".juiced-store.lock")).is_err());
        assert_eq!(
            std::fs::read(foreign.join("config.json")).unwrap(),
            untouched
        );
        assert!(!foreign.join("kv-cache").exists());
        assert!(!foreign.join("artifacts").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sibling_root_loss_blocks_writes_but_preserves_preconditions_and_other_managers() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        let independent = directory.path().join("independent");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&independent).unwrap();
        let original = relative_configuration(&store, "owned");
        relative_configuration(&independent, "independent");
        let manager = Manager::open(store.join("config.json")).unwrap();
        let other = Manager::open(independent.join("config.json")).unwrap();
        let retired = directory.path().join("retired-artifacts");
        std::fs::rename(store.join("artifacts"), &retired).unwrap();
        std::fs::create_dir(store.join("artifacts")).unwrap();
        let sentinel = store.join("artifacts/foreign");
        std::fs::write(&sentinel, b"foreign bytes").unwrap();
        let cache_before = std::fs::read(store.join("kv-cache/snapshots.json")).unwrap();

        assert_eq!(
            manager
                .set_policy("owned", Policy::Disabled, 0)
                .await
                .unwrap_err()
                .code,
            "revision_conflict"
        );
        assert_eq!(
            manager.load("absent").await.unwrap_err().status,
            StatusCode::NOT_FOUND
        );
        for error in [
            manager
                .set_policy("owned", Policy::Disabled, 1)
                .await
                .unwrap_err(),
            manager.configure_cache(true, 0).await.unwrap_err(),
            manager.load("owned").await.unwrap_err(),
        ] {
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error.code, "storage_root_unavailable");
        }
        assert_eq!(manager.catalog().await[0].policy, Policy::OnDemand);
        assert_eq!(std::fs::read(store.join("config.json")).unwrap(), original);
        assert_eq!(
            std::fs::read(store.join("kv-cache/snapshots.json")).unwrap(),
            cache_before
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"foreign bytes");
        assert!(manager.shutdown().await.is_settled());
        other
            .set_policy("independent", Policy::Disabled, 1)
            .await
            .unwrap();
        assert!(other.shutdown().await.is_settled());
    }

    #[cfg(all(unix, feature = "test-fixture"))]
    #[tokio::test]
    async fn supplied_configuration_uses_the_same_claimed_store_and_rejects_an_overlapping_manager()
    {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("store");
        let foreign = directory.path().join("foreign");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let original = relative_configuration(&store, "owned");
        let untouched = relative_configuration(&foreign, "foreign");
        let config: Config = serde_json::from_slice(&original).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&store, &alias).unwrap();
        let manager = Manager::new(config.clone(), alias.join("config.json")).unwrap();
        assert_eq!(std::fs::read(store.join("config.json")).unwrap(), original);
        assert!(Manager::new(config, store.join("config.json")).is_err());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&foreign, &alias).unwrap();
        assert_eq!(
            manager.worker("owned").unwrap().spec.model_file,
            std::fs::canonicalize(&store).unwrap().join("weights.gguf")
        );
        manager
            .set_policy("owned", Policy::Disabled, 1)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(foreign.join("config.json")).unwrap(),
            untouched
        );
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.join("config.json")).unwrap()).unwrap();
        assert_eq!(persisted["models"][0]["policy"], "disabled");
        assert_eq!(persisted["models"][0]["model_file"], "weights.gguf");
    }

    pub(super) fn manager() -> (tempfile::TempDir, Arc<Manager>) {
        let directory = tempfile::tempdir().unwrap();
        let model = directory.path().join("weights.gguf");
        std::fs::write(&model, b"owned preparation test weights").unwrap();
        let config: Config = serde_json::from_value(json!({
            "schema_version":1,"inhibit_suspend":false,"resident_budget_bytes":4096,
            "startup_timeout_ms":1000,"request_timeout_ms":1000,
            "models":[{"id":"one","model_file":model,"provider":"llama-cpp","command":"unstarted-engine",
                "args":["--model","{model}","--port","{port}"],"resident_bytes":1024,"max_inflight":1,
                "operations":["completions"],"policy":"on-demand","revision":1}]
        })).unwrap();
        let manager =
            Manager::assemble(config, directory.path().join("config.json"), None, false).unwrap();
        (directory, manager)
    }

    async fn preparation(manager: &Manager, worker: &Worker) -> Preparation {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let flag = PreparationFlag::begin(worker, deadline).unwrap();
        let permit = manager
            .startup
            .acquire(worker.record_model.clone(), deadline)
            .await
            .unwrap();
        let preparation = Preparation::begin(
            flag,
            manager.budget.reserve(worker.spec.resident_bytes).unwrap(),
            permit,
        );
        let activity = manager
            .power
            .activity(Instant::now() + Duration::from_secs(1))
            .await;
        preparation.resources.claims.lock().unwrap().power = Some(activity);
        preparation
    }

    async fn until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned preparation settles within its test bound");
    }

    fn protected_host_claim(preparation: &Preparation) -> Arc<crate::admission::Admission> {
        let admission = crate::admission::Admission::new(crate::admission::Settings {
            memory: crate::admission::MemorySettings {
                mode: crate::admission::MemoryMode::Protected,
                capacity_bytes: Some(4096),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        admission.seed_host_sample_for_test(Some(4096));
        let claim = admission
            .reserve_host(2048, crate::admission::Priority::Interactive)
            .unwrap();
        *preparation.resources.host.lock().unwrap() = Some(claim);
        admission
    }

    #[tokio::test]
    async fn cancelled_blocking_preparation_keeps_resources_and_blocks_mutation_until_completion() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let preparation = preparation(&manager, &worker).await;
        let host = protected_host_claim(&preparation);
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(blocking_preparation(
            preparation,
            move || {
                let _ = started.send(());
                let _ = wait.recv_timeout(Duration::from_secs(10));
                Ok::<_, ApiError>(())
            },
            "worker_identity",
        ));
        entered.await.unwrap();
        task.abort();
        assert!(task.await.err().unwrap().is_cancelled());
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 1024);
        assert_eq!(host.snapshot().memory.unwrap().outstanding_bytes, 2048);
        assert_eq!(manager.power.snapshot().active, 1);
        assert_eq!(manager.startup.snapshot().active, 1);
        assert_eq!(manager.pending_preparations(), ["one"]);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), manager.load("one"))
                .await
                .is_err()
        );
        assert_eq!(
            manager.unload("one").await.unwrap_err().code,
            "worker_preparation_pending"
        );
        assert_eq!(
            manager
                .set_options("one", 1, Default::default())
                .await
                .unwrap_err()
                .code,
            "worker_in_use"
        );
        assert_eq!(
            manager
                .set_policy("one", Policy::Disabled, 1)
                .await
                .unwrap_err()
                .code,
            "worker_starting"
        );
        {
            let mut state = worker.state.lock().await;
            state.policy = Policy::Disabled;
        }
        assert_eq!(
            manager.unregister("one", 1).await.unwrap_err().code,
            "model_in_use"
        );
        let catalog = manager.catalog().await;
        assert!(catalog[0].preparation_pending);
        assert_eq!(catalog[0].state, "loading");
        assert_eq!(manager.admission()["pending_preparations"], json!(["one"]));
        let shutdown = manager.shutdown().await;
        assert!(!shutdown.is_settled());
        assert!(shutdown.pending_processes.is_empty());
        assert_eq!(shutdown.pending_preparations, ["one"]);
        assert!(!shutdown.power_settled);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 1024);
        assert_eq!(host.snapshot().memory.unwrap().outstanding_bytes, 2048);
        release.send(()).unwrap();
        until(|| !worker.preparing.load(Ordering::Acquire)).await;
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(host.snapshot().memory.unwrap().reserved_bytes, 0);
        assert_eq!(manager.power.snapshot().active, 0);
        assert_eq!(manager.startup.snapshot().active, 0);
        assert!(manager.shutdown().await.is_settled());
    }

    #[tokio::test]
    async fn ready_releases_startup_power_while_residency_remains_owned() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let preparation = preparation(&manager, &worker).await;
        let child_resources = preparation.resources.clone();
        assert_eq!(manager.power.snapshot().active, 1);
        child_resources.release_startup();
        assert!(child_resources.claims.try_lock().unwrap().power.is_none());
        assert_eq!(manager.power.snapshot().active, 0);
        assert_eq!(manager.startup.snapshot().active, 0);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 1024);
        drop(preparation);
        assert!(!worker.preparing.load(Ordering::Acquire));
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 1024);
        child_resources.release_startup();
        drop(child_resources);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_post_spawn_observation_keeps_power_and_residency_after_child_exit_ack() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let preparation = preparation(&manager, &worker).await;
        let host = protected_host_claim(&preparation);
        let plan = test_launch_plan(&manager, &worker);
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let child = manager
            .processes
            .spawn(
                Arc::from("one"),
                &mut command,
                WorkerResources {
                    plan: plan.clone(),
                    _cache: None,
                    speech: None,
                    startup: preparation.resources.clone(),
                    uncertain: None,
                    state: None,
                    writer_model: None,
                    writer_generation: AtomicU64::new(0),
                },
            )
            .unwrap();
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(blocking_preparation(
            preparation,
            move || {
                let _ = started.send(());
                let _ = wait.recv_timeout(Duration::from_secs(10));
                Ok::<_, crate::runtime_identity::Refusal>(())
            },
            "runtime_identity_interrupted",
        ));
        entered.await.unwrap();
        task.abort();
        assert!(task.await.err().unwrap().is_cancelled());
        worker.state.lock().await.policy = Policy::Hot;
        drop(child);
        assert!(manager.processes.snapshot("one").is_some());
        let stopped = manager.unload("one").await.unwrap();
        assert!(stopped.preparation_pending);
        assert!(manager.processes.snapshots().is_empty());
        assert!(worker.preparing.load(Ordering::Acquire));
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 1024);
        assert_eq!(host.snapshot().memory.unwrap().outstanding_bytes, 2048);
        assert_eq!(manager.power.snapshot().active, 1);
        assert_eq!(manager.startup.snapshot().active, 1);
        release.send(()).unwrap();
        until(|| !worker.preparing.load(Ordering::Acquire)).await;
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(host.snapshot().memory.unwrap().reserved_bytes, 0);
        assert_eq!(manager.power.snapshot().active, 0);
        assert_eq!(manager.startup.snapshot().active, 0);
    }

    #[tokio::test]
    async fn queued_policy_change_rejects_original_intent_and_its_joiners_before_resources() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let holder = manager
            .startup
            .acquire(
                Arc::from("held-startup"),
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        let first = manager.load("one");
        tokio::pin!(first);
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        assert_eq!(manager.startup.snapshot().pending, 1);
        let duplicate = manager.load("one");
        tokio::pin!(duplicate);
        assert!(futures_util::poll!(duplicate.as_mut()).is_pending());
        assert_eq!(
            worker
                .attempt
                .lock()
                .unwrap()
                .upgrade()
                .unwrap()
                .completed
                .receiver_count(),
            2
        );
        assert_eq!(manager.startup.snapshot().pending, 1);
        assert!(worker.startup_waiting.load(Ordering::Acquire));
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
        assert!(manager.processes.snapshots().is_empty());
        assert_eq!(
            manager
                .set_options("one", 1, Default::default())
                .await
                .unwrap_err()
                .code,
            "worker_in_use"
        );
        let changed = manager
            .set_policy("one", Policy::Disabled, 1)
            .await
            .unwrap();
        assert_eq!(changed.policy, Policy::Disabled);
        assert!(changed.preparation_pending);
        assert!(changed.startup_waiting);
        assert_eq!(
            manager.unload("one").await.unwrap_err().code,
            "worker_preparation_pending"
        );
        assert_eq!(
            manager
                .unregister("one", changed.revision)
                .await
                .unwrap_err()
                .code,
            "model_in_use"
        );
        assert_eq!(first.await.unwrap_err().code, "model_changed");
        assert_eq!(duplicate.await.unwrap_err().code, "model_changed");
        assert_eq!(manager.startup.snapshot().pending, 0);
        assert_eq!(manager.startup.snapshot().active, 1);
        drop(holder);
        assert!(!worker.preparing.load(Ordering::Acquire));
        assert!(!worker.startup_waiting.load(Ordering::Acquire));
        assert_eq!(manager.startup.snapshot().active, 0);
        assert_eq!(manager.startup.snapshot().pending, 0);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
    }

    #[tokio::test]
    async fn cancelled_queued_owner_and_joiner_refund_only_their_own_waits() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let holder = manager
            .startup
            .acquire(
                Arc::from("held-startup"),
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        let loading = manager.clone();
        let first = tokio::spawn(async move { loading.load("one").await });
        until(|| manager.startup.snapshot().pending == 1).await;
        let loading = manager.clone();
        let duplicate = tokio::spawn(async move { loading.load("one").await });
        until(|| {
            worker
                .attempt
                .lock()
                .unwrap()
                .upgrade()
                .is_some_and(|attempt| attempt.completed.receiver_count() == 2)
        })
        .await;
        duplicate.abort();
        assert!(duplicate.await.err().unwrap().is_cancelled());
        assert!(worker.preparing.load(Ordering::Acquire));
        assert_eq!(manager.startup.snapshot().pending, 1);
        let loading = manager.clone();
        let remaining = tokio::spawn(async move { loading.load("one").await });
        until(|| {
            worker
                .attempt
                .lock()
                .unwrap()
                .upgrade()
                .is_some_and(|attempt| attempt.completed.receiver_count() == 2)
        })
        .await;
        first.abort();
        assert!(first.await.err().unwrap().is_cancelled());
        assert_eq!(
            remaining.await.unwrap().unwrap_err().code,
            "worker_preparation_cancelled"
        );
        assert!(!worker.preparing.load(Ordering::Acquire));
        assert!(!worker.startup_waiting.load(Ordering::Acquire));
        assert_eq!(manager.startup.snapshot().pending, 0);
        assert_eq!(manager.startup.snapshot().active, 1);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
        drop(holder);
        assert_eq!(manager.startup.snapshot().active, 0);
    }

    #[tokio::test]
    async fn granted_startup_refunds_its_permit_when_state_reacquisition_exceeds_the_attempt_bound()
    {
        let (_directory, mut manager) = manager();
        let settings = Arc::get_mut(&mut manager).unwrap();
        settings.startup_wait = Duration::from_millis(250);
        settings.startup_timeout = Duration::from_millis(1);
        let worker = manager.worker("one").unwrap();
        let holder = manager
            .startup
            .acquire(
                Arc::from("held-startup"),
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        let loading = manager.clone();
        let first = tokio::spawn(async move { loading.load("one").await });
        until(|| manager.startup.snapshot().pending == 1).await;
        let loading = manager.clone();
        let duplicate = tokio::spawn(async move { loading.load("one").await });
        until(|| {
            worker
                .attempt
                .lock()
                .unwrap()
                .upgrade()
                .is_some_and(|attempt| attempt.completed.receiver_count() == 2)
        })
        .await;
        let state = worker.state.lock().await;
        drop(holder);
        until(|| {
            let startup = manager.startup.snapshot();
            startup.active == 1 && startup.pending == 0
        })
        .await;
        assert_eq!(
            first.await.unwrap().unwrap_err().code,
            "startup_completion_timeout"
        );
        assert_eq!(
            duplicate.await.unwrap().unwrap_err().code,
            "startup_completion_timeout"
        );
        assert!(!worker.preparing.load(Ordering::Acquire));
        assert_eq!(manager.startup.snapshot().active, 0);
        assert_eq!(manager.startup.snapshot().pending, 0);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
        drop(state);
    }

    #[tokio::test]
    async fn preparation_failure_preserves_configuration_refusal_and_releases_owned_claims() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let preparation = preparation(&manager, &worker).await;
        let (result, preparation) = blocking_preparation(
            preparation,
            || {
                Err::<(), _>(preparation_configuration_error(
                    crate::runtime_inputs::Refusal {
                        code: "unbound_runtime_config",
                        role: Some(crate::runtime_inputs::Role::User),
                        reason: "private diagnostic details",
                    },
                ))
            },
            "worker_identity",
        )
        .await
        .unwrap();
        let error = result.unwrap_err();
        assert_eq!(error.code, "unbound_runtime_config");
        assert!(!error.message.contains("private diagnostic"));
        assert!(fatal_runtime_refusal(&crate::runtime_identity::Refusal {
            status: crate::runtime_identity::Status::Unqualified,
            code: "unbound_runtime_config",
            message: "External runtime configuration cannot be bound to this launch.",
        }));
        assert!(worker.preparing.load(Ordering::Acquire));
        assert_eq!(manager.power.snapshot().active, 1);
        drop(preparation);
        assert!(!worker.preparing.load(Ordering::Acquire));
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
    }
    #[cfg(unix)]
    fn test_launch_plan(manager: &Manager, worker: &Worker) -> Arc<crate::profiles::LaunchPlan> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let executable = crate::identity::executable_path("/bin/sh").unwrap();
        let mut spec = worker.spec.clone();
        spec.execution_profile = crate::execution_profile::ExecutionProfile::from_legacy(
            crate::config::Provider::ExternalHttp,
            None,
            Default::default(),
            executable.to_str().unwrap().into(),
            vec![
                "--model".into(),
                "{model}".into(),
                "--port".into(),
                "{port}".into(),
            ],
        );
        let environment = crate::profiles::CapturedEnvironment::capture(None).unwrap();
        let configuration = environment
            .configuration_inputs(&spec.execution_profile, deadline)
            .unwrap();
        let plan = crate::profiles::LaunchPlan::resolve(
            &spec,
            crate::profiles::VerifiedLaunchBindings {
                model: crate::identity::VerifiedFile::open(&spec.model_file, deadline).unwrap(),
                executable: crate::identity::VerifiedFile::open(&executable, deadline).unwrap(),
                bundle: None,
                artifact: None,
                config_root: None,
                environment,
                port: 1,
                cache_directory: manager.cache_directory.clone(),
                workspace: None,
                public_directory: None,
                configuration,
            },
        )
        .unwrap();
        Arc::new(plan)
    }

    #[cfg(unix)]
    pub(super) async fn speech_child(
        manager: &Arc<Manager>,
        worker: &Arc<Worker>,
        claims: Arc<SpeechClaims>,
    ) -> Running {
        let preparation = preparation(manager, worker).await;
        let plan = test_launch_plan(manager, worker);
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let child = manager
            .processes
            .spawn(
                Arc::from("one"),
                &mut command,
                WorkerResources {
                    plan: plan.clone(),
                    _cache: None,
                    speech: Some(claims),
                    startup: preparation.resources.clone(),
                    uncertain: None,
                    state: Some(Arc::new(state::RetainedStates::default())),
                    writer_model: None,
                    writer_generation: AtomicU64::new(0),
                },
            )
            .unwrap();
        preparation.resources.release_startup();
        drop(preparation);
        Running {
            child,
            plan,
            base: "http://127.0.0.1:1".into(),
            fingerprint: Arc::from("test"),
            runtime_identity: Err(crate::runtime_identity::Refusal {
                status: crate::runtime_identity::Status::Unknown,
                code: "test_unqualified",
                message: "Test child is unqualified.",
            }),
            runtime_summary: Mutex::new(RuntimeSummary {
                state: "unknown",
                qualification: "unqualified",
                closure_digest: None,
                bundle_digest: None,
                process_generation: None,
                reason: Some("test_unqualified"),
                observed_at_unix_ms: 0,
                verified_at_unix_ms: None,
            }),
        }
    }

    pub(super) async fn speech_claims(
        manager: &Arc<Manager>,
        worker: &Arc<Worker>,
    ) -> Arc<SpeechClaims> {
        let slot = worker
            .slots
            .try_acquire(crate::admission::Priority::Interactive)
            .unwrap();
        let power = manager
            .power
            .activity(Instant::now() + Duration::from_secs(5))
            .await;
        worker.active.fetch_add(1, Ordering::AcqRel);
        Arc::new(SpeechClaims(Mutex::new(Some(PhysicalClaims {
            worker: Arc::downgrade(worker),
            _slot: slot,
            _power: power,
            uncertain: false,
        }))))
    }

    #[tokio::test]
    async fn speech_eof_and_pre_dispatch_drop_release_physical_claims_exactly_once() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        for eof in [false, true] {
            let claims = speech_claims(&manager, &worker).await;
            let mut transfer = SpeechTransfer::new(&manager, &worker, 123, claims.clone());
            assert_eq!(worker.active.load(Ordering::Acquire), 1);
            assert_eq!(manager.power.snapshot().active, 1);
            if eof {
                transfer.dispatched = true;
                transfer.settled();
                transfer.settled();
            }
            drop(transfer);
            assert_eq!(worker.active.load(Ordering::Acquire), 0);
            assert_eq!(manager.power.snapshot().active, 0);
            assert!(claims.0.lock().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn repeated_speech_eof_cannot_release_a_later_request_in_the_same_generation() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let claims = speech_claims(&manager, &worker).await;
        let mut transfer = SpeechTransfer::new(&manager, &worker, 123, claims.clone());
        transfer.dispatched = true;
        transfer.settled();
        let later = speech_claims(&manager, &worker).await;
        *claims.0.lock().unwrap() = later.0.lock().unwrap().take();
        transfer.settled();
        drop(transfer);
        assert_eq!(worker.active.load(Ordering::Acquire), 1);
        assert_eq!(manager.power.snapshot().active, 1);
        claims.release();
        assert_eq!(worker.active.load(Ordering::Acquire), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uncertain_speech_retains_claims_while_retirement_waits_then_stops_only_its_generation()
    {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let claims = speech_claims(&manager, &worker).await;
        let run = speech_child(&manager, &worker, claims.clone()).await;
        let generation = run.child.generation();
        let mut state = worker.state.lock().await;
        state.policy = Policy::Hot;
        state.running = Some(run);
        let mut transfer = SpeechTransfer::new(&manager, &worker, generation, claims.clone());
        transfer.dispatched = true;
        drop(transfer);
        tokio::task::yield_now().await;
        assert_eq!(worker.active.load(Ordering::Acquire), 1);
        assert_eq!(manager.power.snapshot().active, 1);
        assert!(
            worker
                .slots
                .try_acquire(crate::admission::Priority::Interactive)
                .is_err()
        );
        assert_eq!(
            manager.processes.snapshot("one").unwrap().generation,
            generation
        );
        drop(state);
        until(|| worker.active.load(Ordering::Acquire) == 0).await;
        assert!(manager.processes.snapshot("one").is_none());
        assert_eq!(manager.power.snapshot().active, 0);
        let state = worker.state.lock().await;
        assert_eq!(state.policy, Policy::Hot);
        assert!(state.running.is_none());
        assert_eq!(state.error, Some("speech_transfer_uncertain"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn speech_eof_and_stale_generation_do_not_retire_the_loaded_child_or_bypass_hot_guard() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let claims = speech_claims(&manager, &worker).await;
        let run = speech_child(&manager, &worker, claims.clone()).await;
        let generation = run.child.generation();
        let pid = run.child.id();
        {
            let mut state = worker.state.lock().await;
            state.running = Some(run);
            state.policy = Policy::Hot;
        }
        assert_eq!(manager.unload("one").await.unwrap_err().code, "worker_busy");
        assert!(!manager.catalog().await[0].cleanup_retry_available);
        let mut transfer = SpeechTransfer::new(&manager, &worker, generation, claims);
        transfer.dispatched = true;
        transfer.settled();
        drop(transfer);
        manager
            .retire_speech_generation(&worker, generation + 1)
            .await;
        assert_eq!(
            worker
                .state
                .lock()
                .await
                .running
                .as_ref()
                .unwrap()
                .child
                .id(),
            pid
        );
        assert_eq!(manager.unload("one").await.unwrap_err().code, "worker_hot");
        worker.state.lock().await.policy = Policy::OnDemand;
        manager.unload("one").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_speech_stop_keeps_physical_claims_until_manual_exact_cleanup_even_when_hot() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let claims = speech_claims(&manager, &worker).await;
        let run = speech_child(&manager, &worker, claims).await;
        let generation = run.child.generation();
        {
            let mut state = worker.state.lock().await;
            state.running = Some(run);
            state.policy = Policy::Hot;
        }
        manager.processes.test_fail_kill(true);
        manager.retire_speech_generation(&worker, generation).await;
        manager.processes.test_background_finished().await;
        assert_eq!(worker.active.load(Ordering::Acquire), 1);
        assert_eq!(manager.power.snapshot().active, 1);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 1024);
        assert!(
            worker
                .slots
                .try_acquire(crate::admission::Priority::Interactive)
                .is_err()
        );
        let observation = manager.processes.snapshot("one").unwrap();
        assert_eq!(observation.generation, generation);
        assert_eq!(observation.last_error, Some("process_stop"));
        assert!(manager.catalog().await[0].cleanup_retry_available);
        assert_eq!(
            manager.unload("one").await.unwrap_err().code,
            "process_stop"
        );
        assert_eq!(worker.active.load(Ordering::Acquire), 1);
        manager.processes.test_fail_kill(false);
        manager.unload("one").await.unwrap();
        assert_eq!(worker.active.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(worker.state.lock().await.policy, Policy::Hot);
        assert!(!manager.catalog().await[0].cleanup_retry_available);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_speech_cleanup_and_unconfirmed_exit_retain_original_generation_claims() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let claims = speech_claims(&manager, &worker).await;
        let run = speech_child(&manager, &worker, claims).await;
        let generation = run.child.generation();
        {
            let mut state = worker.state.lock().await;
            state.running = Some(run);
            state.policy = Policy::Hot;
        }
        manager.processes.test_pause_stop(true);
        manager.processes.test_withhold_ack(true);
        let cleanup_manager = manager.clone();
        let cleanup_worker = worker.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_manager
                .retire_speech_generation(&cleanup_worker, generation)
                .await;
        });
        manager.processes.test_stop_entered().await;
        assert!(!manager.catalog().await[0].cleanup_retry_available);
        cleanup.abort();
        assert!(cleanup.await.unwrap_err().is_cancelled());
        manager.processes.test_pause_stop(false);
        // SIGKILL can already have completed, but its acknowledgement remains
        // unavailable. The physical claims must follow the retained owner.
        assert_eq!(worker.active.load(Ordering::Acquire), 1);
        assert_eq!(manager.power.snapshot().active, 1);
        assert_eq!(
            manager.processes.snapshot("one").unwrap().generation,
            generation
        );
        manager.processes.test_withhold_ack(false);
        manager.processes.test_background_finished().await;
        assert_eq!(worker.active.load(Ordering::Acquire), 0);
        assert_eq!(manager.power.snapshot().active, 0);
        assert_eq!(manager.budget.used.load(Ordering::Acquire), 0);
        assert_eq!(worker.state.lock().await.policy, Policy::Hot);
    }

    #[tokio::test]
    async fn uncertain_speech_without_an_available_cleanup_owner_keeps_generation_claims() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let claims = speech_claims(&manager, &worker).await;
        let mut transfer = SpeechTransfer::new(&manager, &worker, 123, claims.clone());
        transfer.dispatched = true;
        drop(manager);
        drop(transfer);
        tokio::task::yield_now().await;
        assert_eq!(worker.active.load(Ordering::Acquire), 1);
        assert!(claims.0.lock().unwrap().is_some());
        // This represents the generation resource owner acknowledging exit.
        drop(claims);
        assert_eq!(worker.active.load(Ordering::Acquire), 0);
    }
}

struct Running {
    child: crate::process::ManagedChild<WorkerResources>,
    plan: Arc<crate::profiles::LaunchPlan>,
    base: String,
    fingerprint: Arc<str>,
    runtime_identity:
        Result<Arc<crate::runtime_identity::Observation>, crate::runtime_identity::Refusal>,
    runtime_summary: Mutex<RuntimeSummary>,
}

struct WorkerState {
    policy: Policy,
    revision: u64,
    running: Option<Running>,
    error: Option<&'static str>,
}

struct Worker {
    processes: Arc<crate::process::Registry<WorkerResources>>,
    record_model: Arc<str>,
    slots: crate::admission::SlotPool,
    _artifact: Option<Arc<crate::artifacts::ArtifactLease>>,
    spec: ModelSpec,
    protected_memory: bool,
    state: Arc<AsyncMutex<WorkerState>>,
    active: AtomicU32,
    uncertain_requests: AtomicU32,
    uncertain_state_operations: AtomicU32,
    physical_invariant_failed: AtomicBool,
    speech_retiring: AtomicU64,
    preparing: Arc<AtomicBool>,
    startup_waiting: Arc<AtomicBool>,
    attempt: Mutex<Weak<PreparationFlag>>,
    observation: Mutex<Option<ModelView>>,
    cache_identity: Mutex<Option<SnapshotIdentity>>,
    sessions: sessions::SlotTable,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSummary {
    state: &'static str,
    qualification: &'static str,
    closure_digest: Option<Arc<str>>,
    bundle_digest: Option<Arc<str>>,
    process_generation: Option<crate::runtime_identity::ProcessGeneration>,
    reason: Option<&'static str>,
    observed_at_unix_ms: u64,
    verified_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelView {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_set: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_package: Option<String>,
    pub implementation_id: Option<&'static str>,
    pub policy: Policy,
    pub revision: u64,
    pub state: &'static str,
    pub downloaded: bool,
    pub loaded: bool,
    pub pid: Option<u32>,
    pub active_requests: u32,
    pub max_inflight: u32,
    pub resident_bytes: Option<u64>,
    pub operations: Vec<Operation>,
    pub error: Option<&'static str>,
    pub runtime_identity: Option<RuntimeSummary>,
    pub process_lifecycle: Option<crate::process::Observation>,
    pub preparation_pending: bool,
    pub startup_waiting: bool,
    pub cleanup_retry_available: bool,
    pub uncertain_requests: u32,
    pub uncertain_state_operations: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_limits: Option<crate::resource_profile::Limits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residency: Option<ResidencyView>,
    pub vision: bool,
}

#[derive(Clone, Serialize)]
pub struct RequestRecord {
    pub id: u64,
    pub model: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<Arc<str>>,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub status: Option<u16>,
    pub elapsed_ms: u64,
    pub queue_wait_ms: u64,
    pub completed_at_ms: u64,
    #[serde(flatten)]
    pub usage: crate::engine_usage::Observation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_cache: Option<SessionRecord>,
}

#[derive(Clone, Serialize)]
pub struct LastInference {
    pub request_id: u64,
    pub model: Arc<str>,
    pub operation: &'static str,
    pub completed_at_ms: u64,
    pub elapsed_ms: u64,
    #[serde(flatten)]
    pub usage: crate::engine_usage::Observation,
}

pub struct RequestLease {
    worker: Arc<Worker>,
    manager: Arc<Manager>,
    started: Instant,
    record: Option<RequestRecord>,
    cache_identity: Option<SnapshotIdentity>,
    fingerprint: Arc<str>,
    generation: u64,
    session: Option<sessions::Ticket>,
    _physical: Option<PhysicalClaims>,
    speech: Option<SpeechTransfer>,
    protected: Option<physical::ProtectedTransfer>,
    upstream_timeout: Duration,
}

struct PhysicalClaims {
    worker: Weak<Worker>,
    _slot: crate::admission::Slot,
    _power: crate::power::Activity,
    uncertain: bool,
}

impl Drop for PhysicalClaims {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.upgrade() {
            worker.active.fetch_sub(1, Ordering::AcqRel);
            if self.uncertain {
                worker.uncertain_requests.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

#[derive(Default)]
struct SpeechClaims(Mutex<Option<PhysicalClaims>>);

impl SpeechClaims {
    fn release(&self) {
        let claims = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        drop(claims);
    }
}

struct SpeechTransfer {
    claims: Arc<SpeechClaims>,
    completion: Option<tokio::sync::oneshot::Sender<bool>>,
    dispatched: bool,
}

impl SpeechTransfer {
    fn new(
        manager: &Arc<Manager>,
        worker: &Arc<Worker>,
        generation: u64,
        claims: Arc<SpeechClaims>,
    ) -> Self {
        let (completion, wait) = tokio::sync::oneshot::channel();
        let manager = Arc::downgrade(manager);
        let owner = Arc::downgrade(worker);
        tokio::spawn(async move {
            if wait.await != Ok(false)
                && let (Some(manager), Some(worker)) = (manager.upgrade(), owner.upgrade())
            {
                manager.retire_speech_generation(&worker, generation).await;
            }
        });
        Self {
            claims,
            completion: Some(completion),
            dispatched: false,
        }
    }

    fn settled(&mut self) {
        if let Some(completion) = self.completion.take() {
            self.claims.release();
            let _ = completion.send(false);
        }
    }
}

impl Drop for SpeechTransfer {
    fn drop(&mut self) {
        if !self.dispatched {
            self.settled();
        } else if let Some(completion) = self.completion.take() {
            let _ = completion.send(true);
        }
    }
}

impl RequestLease {
    pub fn text_dispatched(&mut self) {
        if let Some(transfer) = self.protected.as_mut() {
            transfer.dispatched();
        }
    }

    pub fn text_reply_eof(&mut self) {
        if let Some(transfer) = self.protected.as_mut() {
            transfer.settled();
        }
    }

    pub fn upstream_timeout(&self) -> Duration {
        self.upstream_timeout
    }

    pub fn adapter(&self) -> crate::config::Provider {
        self.worker.spec.execution_profile.adapter
    }

    pub fn upstream_model(&self) -> Option<&str> {
        let profile = &self.worker.spec.execution_profile;
        (profile.adapter == crate::config::Provider::FastFlowLm)
            .then(|| crate::flm::checkpoint(&profile.args))
            .flatten()
    }

    pub fn speech_dispatched(&mut self) {
        if let Some(speech) = self.speech.as_mut() {
            speech.dispatched = true;
        }
    }

    pub fn speech_reply_eof(&mut self) {
        if let Some(speech) = self.speech.as_mut() {
            speech.settled();
        }
    }

    pub fn finish(&mut self, outcome: &'static str, status: Option<u16>) {
        if let Some(record) = self.record.as_mut() {
            record.outcome = outcome;
            record.status = status;
        }
    }
    pub fn observe_usage(&mut self, usage: crate::engine_usage::Observation) {
        if let Some(record) = self.record.as_mut() {
            record.usage = usage;
        }
    }
    pub fn bind_cache(&mut self, context: &RequestContext, session: &str, protocol: &str) {
        self.cache_identity = Some(snapshot_identity(
            &self.fingerprint,
            context,
            session,
            protocol,
        ));
    }
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        let Some(mut record) = self.record.take() else {
            return;
        };
        let completed = record.outcome == "completed"
            && record
                .status
                .is_some_and(|status| (200..300).contains(&status));
        if let Some(ticket) = self.session.take() {
            record.session_cache = Some(ticket.record);
            ticket.settle(&self.manager, &self.worker, completed, &record.usage);
        }
        self.manager.residency.touch(&self.worker.spec.id);
        if completed {
            *self
                .worker
                .cache_identity
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = self.cache_identity.take();
        }
        record.elapsed_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        record.completed_at_ms = unix_time_ms();
        if record.outcome == "completed" && !record.usage.is_empty() {
            self.manager.usage_totals.observe(&record.usage);
            *self
                .manager
                .last_inference
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(LastInference {
                request_id: record.id,
                model: record.model.clone(),
                operation: record.operation,
                completed_at_ms: record.completed_at_ms,
                elapsed_ms: record.elapsed_ms,
                usage: record.usage,
            });
        }
        self.manager.metrics.observe(&record.model, &record.usage);
        let mut records = self
            .manager
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if records.len() == 128 {
            records.pop_front();
        }
        records.push_back(record);
    }
}

fn process_error(error: crate::process::Error) -> ApiError {
    ApiError::new(
        StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        error.code,
        error.message,
    )
}

fn snapshot_identity(
    fingerprint: &str,
    context: &RequestContext,
    session: &str,
    protocol: &str,
) -> SnapshotIdentity {
    SnapshotIdentity {
        execution_fingerprint: fingerprint.to_owned(),
        principal: context.principal.to_owned(),
        credential_epoch: context.credential_epoch.to_string(),
        protocol: protocol.to_owned(),
        session_digest: format!("{:x}", Sha256::digest(session.as_bytes())),
    }
}
fn cache_error(error: CacheError) -> ApiError {
    let status = match error {
        CacheError::RootUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        CacheError::Persistence(ref error) if error.root_unavailable() => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        CacheError::NotFound => StatusCode::NOT_FOUND,
        CacheError::Invalid(_) => StatusCode::BAD_REQUEST,
        CacheError::Conflict
        | CacheError::Busy
        | CacheError::IdentityMismatch
        | CacheError::Uncertain => StatusCode::CONFLICT,
        CacheError::Limit => StatusCode::INSUFFICIENT_STORAGE,
        CacheError::Cancelled => StatusCode::REQUEST_TIMEOUT,
        CacheError::Deadline => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError::new(status, error.code(), error.public_message())
}

fn storage_root_error() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_root_unavailable",
        "A claimed storage root is unavailable. Restore its owned path before retrying.",
    )
}

fn configuration_size_error(error: crate::config::ConfigSizeError) -> ApiError {
    match error {
        crate::config::ConfigSizeError::TooLarge => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "configuration_too_large",
            "This update would exceed the 16 MiB saved configuration limit. Remove unused registrations or reduce advanced arguments before retrying.",
        ),
        crate::config::ConfigSizeError::Serialization(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_serialization",
            "The configuration could not be serialized. Its saved state is unchanged.",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheSave {
    pub model: String,
    pub slot: u32,
    pub session: String,
    pub expected_generation: u64,
    pub maximum_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheRestore {
    pub model: String,
    pub slot: u32,
    pub session: String,
    pub snapshot: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheForget {
    pub snapshot: String,
    pub session: String,
    pub expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheRecovery {
    pub snapshot: String,
    pub session: String,
    pub expected_revision: u64,
    pub reader: Option<String>,
}

#[derive(Serialize)]
pub struct CacheView {
    pub schema_version: u32,
    pub enabled: bool,
    pub configuration_generation: u64,
    pub generation: u64,
    pub data: Vec<crate::cache::SnapshotMetadata>,
    pub page: crate::cache::PageInfo,
    pub qualification: &'static str,
    pub scope: &'static str,
}

#[derive(Debug)]
pub struct ShutdownReport {
    pub pending_processes: Vec<crate::process::Observation>,
    pub pending_preparations: Vec<String>,
    pub worker_stops_settled: bool,
    pub artifact_writers_settled: bool,
    pub power_settled: bool,
    pub configuration_settled: bool,
    pub native_writers_settled: bool,
    pub host_memory_settled: bool,
    pub physical_claims_settled: bool,
    pub cache_operations_settled: bool,
    pub runtime_operations_settled: bool,
}

impl ShutdownReport {
    pub fn is_settled(&self) -> bool {
        self.pending_processes.is_empty()
            && self.pending_preparations.is_empty()
            && self.worker_stops_settled
            && self.artifact_writers_settled
            && self.power_settled
            && self.configuration_settled
            && self.native_writers_settled
            && self.host_memory_settled
            && self.physical_claims_settled
            && self.cache_operations_settled
            && self.runtime_operations_settled
    }
}

struct NativeSettingsSnapshot {
    generation: Arc<crate::native_store::generations::PreparedBinding>,
    context: Arc<crate::native_execution::ResolutionContext>,
}

impl NativeSettingsSnapshot {
    fn new(
        generation: crate::native_store::generations::PreparedBinding,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let context = crate::native_execution::ResolutionContext::from_binding(&generation)?;
        Ok(Arc::new(Self {
            generation: Arc::new(generation),
            context: Arc::new(context),
        }))
    }
}

struct NativeSettingsState {
    store: Arc<crate::native_store::generations::Store>,
    current: RwLock<Option<Arc<NativeSettingsSnapshot>>>,
}

pub struct Manager {
    runtime_definitions: crate::catalog::Catalog,
    hardware: crate::hardware::Detector,
    engine_jobs: engines::EngineJobs,
    native_adoption: native_adoption::Status,
    flm_jobs: flm::FlmJobs,
    engine_releases: crate::engines::ReleaseEndpoints,
    runtime_packages: std::sync::OnceLock<Arc<crate::runtime_packages::Store>>,
    runtime_importing: AtomicBool,
    runtime_control: Arc<crate::runtime_packages::memory::Controller>,
    runtime_projection: axum::body::Bytes,
    admission: Arc<crate::admission::Admission>,
    startup: Arc<crate::startup::StartupGate>,
    ingress: Arc<crate::admission::Ingress>,
    power: Arc<crate::power::Controller>,
    processes: Arc<crate::process::Registry<WorkerResources>>,
    workers: RwLock<BTreeMap<String, Arc<Worker>>>,
    budget: Arc<Budget>,
    config: Arc<AsyncMutex<Config>>,
    aliases: RwLock<Arc<crate::config::ModelAliases>>,
    publications: Arc<publication::Publications>,
    config_path: PathBuf,
    config_root: Option<crate::storage::DirectoryLease>,
    live_roots: Option<Arc<crate::storage::LiveRoots>>,
    native_settings: Option<NativeSettingsState>,
    native_import_only: bool,
    host_memory: host::HostMemory,
    #[cfg(test)]
    publication_probe: Mutex<Option<Arc<publication::Probe>>>,
    cache: Option<Arc<CacheStore>>,
    cache_memory_unavailable: Option<&'static str>,
    cache_operations: Arc<state::Operations>,
    session_settings: RwLock<Arc<crate::sessions::Settings>>,
    session_used: Mutex<std::collections::HashMap<String, u64>>,
    session_lineage: Arc<sessions::Lineages>,
    session_saves: Arc<tokio::sync::Semaphore>,
    artifacts: Option<Arc<crate::artifacts::ArtifactStore>>,
    cache_directory: PathBuf,
    pub client: reqwest::Client,
    startup_timeout: Duration,
    startup_wait: Duration,
    pub request_timeout: Duration,
    records: Mutex<VecDeque<RequestRecord>>,
    last_inference: Mutex<Option<LastInference>>,
    usage_totals: llama::UsageTotals,
    logs: Arc<crate::logs::LogBuffer>,
    metrics: crate::metrics::Counters,
    cors: crate::cors::Policy,
    system_stats: crate::system_stats::Sampler,
    library: Arc<crate::library::Library>,
    next_request: AtomicU64,
    shutting_down: std::sync::atomic::AtomicBool,
    paused: std::sync::atomic::AtomicBool,
    residency: residency::Residency,
    protocol_gate: Arc<crate::protocol_access::Gate>,
}

impl Manager {
    async fn verify_storage(&self) -> Result<(), ApiError> {
        let Some(scope) = self.live_roots.clone() else {
            return Ok(());
        };
        let packages = self.runtime_packages.get().cloned();
        tokio::task::spawn_blocking(move || {
            scope.verify().map_err(|_| storage_root_error())?;
            if let Some(packages) = packages {
                packages
                    .verify()
                    .map_err(runtime_selection::package_error)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| storage_root_error())?
    }

    pub fn open(config_path: PathBuf) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let binding = ConfigurationBinding::claim(config_path)?;
        let config = binding.read()?;
        Self::assemble(config, binding.path, Some(binding.root), false)
    }

    pub fn open_for_native_import(
        config_path: PathBuf,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        if !cfg!(unix) {
            return Err("Native metadata import needs a qualified private-permission implementation on this platform".into());
        }
        let binding = ConfigurationBinding::claim(config_path)?;
        let config = binding.read()?;
        if config.native_settings.is_some() {
            return Err("This configuration already owns imported native settings".into());
        }
        Self::assemble(config, binding.path, Some(binding.root), true)
    }

    #[cfg(feature = "test-fixture")]
    pub fn new(
        config: Config,
        config_path: PathBuf,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        config.validate()?;
        let binding = ConfigurationBinding::claim(config_path)?;
        match binding
            .root
            .read_bytes(binding.filename(), crate::config::MAXIMUM_CONFIG_BYTES)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Self::assemble(config, binding.path, Some(binding.root), false)
    }

    fn assemble(
        config: Config,
        config_path: PathBuf,
        config_root: Option<crate::storage::DirectoryLease>,
        prepare_native: bool,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        config.validate()?;
        if let Some(root) = &config_root {
            root.verify()?;
            if config_path.parent() != Some(root.root()) {
                return Err(
                    "Configuration path must remain within its claimed canonical directory".into(),
                );
            }
        }
        let runtime_definitions = crate::catalog::Catalog::from_seed()?;
        let runtime_projection =
            axum::body::Bytes::from(serde_json::to_vec(&runtime_definitions.projection())?);
        let admission = crate::admission::Admission::new(config.admission.clone())?;
        let runtime_control = crate::runtime_packages::memory::Controller::new(admission.clone());
        let host_memory = host::HostMemory::new();
        let mut cache_memory_unavailable = None;
        if admission.memory_protected() {
            host_memory.sample_bootstrap(&admission);
        }
        let cache_directory = crate::cache::root::store_root(config_root.as_ref().map_or_else(
            || config_path.parent().expect("configuration parent"),
            |root| root.root(),
        ));
        let mut cache_notice = None;
        let (live_roots, cache, artifacts, native_settings) = if let Some(root) = &config_root {
            root.verify()?;
            let artifact_root = root.claim(root.root().join("artifacts"))?;
            root.verify()?;
            artifact_root.verify()?;
            let preparation = crate::cache::root::prepare(root.root())?;
            cache_notice = preparation.notice(root.root()).map(|notice| {
                let level = if preparation == crate::cache::root::Preparation::Moved {
                    crate::logs::Level::Info
                } else {
                    crate::logs::Level::Warning
                };
                (level, notice)
            });
            let cache_root = root.claim(&cache_directory)?;
            let mut roots = vec![root.clone(), artifact_root.clone(), cache_root.clone()];
            let bootstrap_scope = crate::storage::LiveRoots::new(roots.clone())?;
            let native_root = if prepare_native || config.native_settings.is_some() {
                let private = root.acquire_private_child(
                    &bootstrap_scope,
                    std::path::Path::new(crate::native_store::descriptor::STORE_ROOT),
                    prepare_native,
                )?;
                roots.push(private.clone());
                Some(private)
            } else {
                None
            };
            let scope = crate::storage::LiveRoots::new(roots)?;
            let native_settings = if let Some(native_root) = native_root {
                let (store, current) = crate::native_store::generations::Store::open_bound(
                    native_root,
                    scope.clone(),
                    config.native_settings.as_ref(),
                    prepare_native,
                )?;
                Some(NativeSettingsState {
                    store: Arc::new(store),
                    current: RwLock::new(current.map(NativeSettingsSnapshot::new).transpose()?),
                })
            } else {
                None
            };
            let limits = crate::cache::CacheLimits::default();
            let cache = if admission.memory_protected() {
                match crate::cache::memory::Bootstrap::reserve(&admission, &limits) {
                    Ok(bootstrap) => Some(CacheStore::open_accounted(
                        cache_root,
                        limits,
                        scope.clone(),
                        bootstrap,
                    )?),
                    Err(error) => {
                        cache_memory_unavailable = Some(error.code);
                        None
                    }
                }
            } else {
                Some(CacheStore::open_scoped(cache_root, limits, scope.clone())?)
            };
            let artifacts = crate::artifacts::ArtifactStore::open_scoped(
                artifact_root,
                crate::artifacts::ArtifactLimits::default(),
                config.artifact_source_policy,
                scope.clone(),
            )?;
            (Some(scope), cache, Some(artifacts), native_settings)
        } else {
            (None, None, None, None)
        };
        let runtime_packages = config_root
            .as_ref()
            .map(|root| {
                crate::runtime_packages::Store::open_optional_accounted(
                    root.clone(),
                    live_roots.as_ref().expect("configuration roots").clone(),
                    runtime_control.clone(),
                )
            })
            .transpose()?
            .flatten();
        let startup = crate::startup::StartupGate::new(config.admission.startup.clone())?;
        let ingress = crate::admission::Ingress::new(config.admission.ingress_body_bytes);
        let processes = crate::process::Registry::new(crate::process::DEFAULT_STOP_TIMEOUT);
        let mut workers = BTreeMap::new();
        for mut spec in
            config.resolved_models(config_path.parent().unwrap_or(std::path::Path::new(".")))
        {
            let held = match &spec.artifact {
                Some(id) => {
                    let store = artifacts
                        .as_ref()
                        .ok_or("Managed artifact storage is required")?;
                    let held = store.hold_binding_with(
                        id,
                        spec.artifact_set.as_deref(),
                        spec.execution_profile.adapter.accepts_companions(),
                    )?;
                    let reference = format!("model:{}", spec.id);
                    if let Some(previous) = &spec.previous_artifact_set {
                        let _previous =
                            store.hold_binding(&previous.artifact, Some(&previous.artifact_set))?;
                        store.reference_bindings(
                            &reference,
                            &[
                                crate::artifacts::ArtifactBinding {
                                    artifact: id.clone(),
                                    artifact_set: spec
                                        .artifact_set
                                        .clone()
                                        .expect("validated current set"),
                                },
                                previous.as_ref().clone(),
                            ],
                            store.generation(),
                        )?;
                    } else if held.set_id().is_some()
                        || !held
                            .artifact()
                            .references
                            .iter()
                            .any(|value| value == &reference)
                    {
                        store.reference_binding(
                            id,
                            held.set_id(),
                            &reference,
                            true,
                            store.generation(),
                        )?;
                    }
                    spec.model_file = held.path().to_owned();
                    Some(held)
                }
                None => None,
            };
            if spec.execution_profile.native_model_id().is_some() {
                let native = native_settings
                    .as_ref()
                    .ok_or("Native settings storage is required")?;
                let current = native
                    .current
                    .read()
                    .map_err(|_| "Native settings ownership is unavailable")?
                    .as_ref()
                    .cloned()
                    .ok_or("The native settings generation is missing")?;
                let artifact = held
                    .as_ref()
                    .ok_or("Native runtime bindings require a managed model artifact")?;
                Self::bind_native_model(&mut spec, artifact, current)?;
            }
            if admission.memory_protected() && spec.execution_profile.runtime_package().is_some() {
                host_memory.sample_bootstrap(&admission);
            }
            spec.execution_profile.bind_runtime_package(
                runtime_packages.as_ref(),
                Instant::now() + Duration::from_millis(config.startup_timeout_ms),
            )?;
            workers.insert(
                spec.id.clone(),
                Self::create_worker(spec, held.map(Arc::new), &admission, &processes, None)?,
            );
        }
        let runtime_packages = {
            let slot = std::sync::OnceLock::new();
            if let Some(store) = runtime_packages {
                let _ = slot.set(store);
            }
            slot
        };
        let power = crate::power::Controller::new(config.inhibit_suspend);
        let (log_threshold, log_warning) =
            crate::logs::Threshold::resolve(config.log_level.as_deref());
        let cors = crate::cors::Policy::resolve(config.allowed_origins.as_deref());
        let library = storage::library(
            &config,
            config_root.as_ref(),
            live_roots.as_ref(),
            artifacts.as_ref(),
        );
        Ok(Arc::new(Self {
            runtime_definitions,
            hardware: crate::hardware::Detector::from_environment(),
            engine_jobs: engines::EngineJobs::default(),
            native_adoption: native_adoption::Status::default(),
            flm_jobs: flm::FlmJobs::default(),
            engine_releases: crate::engines::ReleaseEndpoints::from_environment(),
            runtime_packages,
            runtime_importing: AtomicBool::new(false),
            runtime_control,
            runtime_projection,
            admission,
            startup,
            ingress,
            power,
            processes,
            workers: RwLock::new(workers),
            budget: Arc::new(Budget {
                limit: config.resident_budget_bytes,
                used: AtomicU64::new(0),
            }),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(2))
                .build()?,
            startup_timeout: Duration::from_millis(config.startup_timeout_ms),
            startup_wait: Duration::from_millis(config.admission.startup.wait_ms),
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            aliases: RwLock::new(Arc::new(config.aliases.clone())),
            session_settings: RwLock::new(Arc::new(config.session_cache.clone())),
            session_used: Mutex::new(std::collections::HashMap::new()),
            session_lineage: Arc::new(sessions::Lineages::load(config_root.as_ref())),
            session_saves: Arc::new(tokio::sync::Semaphore::new(2)),
            residency: residency::Residency::new(&config),
            protocol_gate: Arc::new(crate::protocol_access::Gate::new(&config.protocols)),
            config: Arc::new(AsyncMutex::new(config)),
            publications: publication::Publications::new(),
            config_path,
            config_root,
            live_roots,
            native_settings,
            native_import_only: prepare_native,
            host_memory,
            #[cfg(test)]
            publication_probe: Mutex::new(None),
            cache,
            cache_memory_unavailable,
            cache_operations: Arc::default(),
            artifacts,
            cache_directory,
            records: Mutex::new(VecDeque::new()),
            last_inference: Mutex::new(None),
            usage_totals: Default::default(),
            logs: {
                let logs = crate::logs::LogBuffer::new();
                // Unit tests share one process stderr; only the service binary mirrors.
                if !cfg!(test) {
                    logs.mirror_to_stderr(log_threshold);
                }
                if let Some(warning) = log_warning {
                    logs.service(crate::logs::Level::Warning, &warning);
                }
                if let Some((level, notice)) = &cache_notice {
                    logs.service(*level, notice);
                }
                logs
            },
            metrics: crate::metrics::Counters::default(),
            cors,
            system_stats: crate::system_stats::Sampler::default(),
            library,
            next_request: AtomicU64::new(1),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            paused: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub async fn system_info(&self) -> Result<axum::body::Bytes, ApiError> {
        let snapshot = self
            .hardware
            .snapshot()
            .await
            .map_err(|error| match error {
                crate::hardware::DetectionError::Deadline => ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "hardware_detection_deadline",
                    "Hardware detection did not finish within its deadline. Retry later.",
                ),
                crate::hardware::DetectionError::Interrupted => ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "hardware_detection_interrupted",
                    "Hardware detection was interrupted. Retry later.",
                ),
            })?;
        let document = crate::hardware::SystemInfo::new(
            &snapshot,
            self.hardware.ttl(),
            &self.runtime_definitions,
        );
        serde_json::to_vec(&document)
            .map(axum::body::Bytes::from)
            .map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "hardware_response",
                    "Hardware observations could not be represented.",
                )
            })
    }

    pub fn runtime_catalog(&self) -> axum::body::Bytes {
        self.runtime_projection.clone()
    }

    pub fn resolve_runtime(
        &self,
        recipe: &str,
        backend: &str,
    ) -> Result<crate::catalog::LegacyResolution, ApiError> {
        self.runtime_definitions
            .resolve_legacy(recipe, backend)
            .map_err(|error| {
                ApiError::new(
                    if error.code == "unknown_legacy_recipe" {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::BAD_REQUEST
                    },
                    error.code,
                    error.message,
                )
            })
    }

    pub fn library(&self) -> &Arc<crate::library::Library> {
        &self.library
    }

    pub fn artifacts(&self) -> Result<Arc<crate::artifacts::ArtifactStore>, ApiError> {
        self.artifacts.clone().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "artifact_store_unavailable",
                "Managed artifact storage is unavailable.",
            )
        })
    }

    fn create_worker(
        spec: ModelSpec,
        artifact: Option<Arc<crate::artifacts::ArtifactLease>>,
        admission: &Arc<crate::admission::Admission>,
        processes: &Arc<crate::process::Registry<WorkerResources>>,
        slots: Option<crate::admission::SlotPool>,
    ) -> Result<Arc<Worker>, ApiError> {
        let spec = fit::refresh_reservation(spec);
        let state = WorkerState {
            policy: spec.policy,
            revision: spec.revision,
            running: None,
            error: None,
        };
        let max_inflight = spec.max_inflight;
        let worker = Arc::new(Worker {
            processes: processes.clone(),
            record_model: Arc::from(spec.id.as_str()),
            slots: match slots {
                Some(slots) => slots,
                None => admission
                    .pool(
                        spec.id.as_str(),
                        Some(spec.max_inflight),
                        spec.essential_headroom,
                    )
                    .map_err(ApiError::from)?,
            },
            _artifact: artifact,
            state: Arc::new(AsyncMutex::new(state)),
            spec,
            protected_memory: admission.memory_protected(),
            active: AtomicU32::new(0),
            uncertain_requests: AtomicU32::new(0),
            uncertain_state_operations: AtomicU32::new(0),
            physical_invariant_failed: AtomicBool::new(false),
            speech_retiring: AtomicU64::new(0),
            preparing: Arc::new(AtomicBool::new(false)),
            startup_waiting: Arc::new(AtomicBool::new(false)),
            attempt: Mutex::new(Weak::new()),
            observation: Mutex::new(None),
            cache_identity: Mutex::new(None),
            sessions: sessions::SlotTable::new(max_inflight),
        });
        let state = worker.state.try_lock().expect("new worker is unlocked");
        Self::publish(&worker, &state);
        drop(state);
        Ok(worker)
    }

    pub async fn options(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        if self
            .worker(id)?
            .spec
            .execution_profile
            .native_model_id()
            .is_some()
        {
            return self.native_options(id).await;
        }
        let config = self.config.lock().await;
        let spec = config
            .models
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_model",
                    "The model is not registered.",
                )
            })?;
        let effective = spec
            .execution_profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?
            .1;
        let mut view = serde_json::json!({"model":id,"revision":spec.revision,"provider":spec.execution_profile.adapter,"saved":spec.execution_profile.options(),"effective_launch":effective,"physical_slots":spec.max_inflight,"reservation_bytes":self.worker(id)?.spec.resident_bytes,"context_semantics":"context_tokens is a total launch budget; serving context requires engine observation"});
        if spec.execution_profile.adapter == crate::config::Provider::StableDiffusionCpp {
            view["image_defaults"] =
                crate::media::image_defaults_view(&spec.execution_profile.args);
        }
        drop(config);
        if let Some((fields, metrics)) = self.launch_provenance(id).await {
            view["fields"] = fields;
            view["mtp_metrics"] = metrics;
        }
        Ok(view)
    }

    async fn profile_observation(
        &self,
        base: &str,
    ) -> Result<
        (
            crate::profiles::PropsObservation,
            Vec<crate::profiles::SlotObservation>,
        ),
        ApiError,
    > {
        let read = |path: &'static str| async move {
            let mut response = self
                .client
                .get(format!("{base}{path}"))
                .timeout(Duration::from_millis(500))
                .send()
                .await
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "profile_observation_unavailable",
                        "The engine did not provide bounded profile observations.",
                    )
                })?;
            if !response.status().is_success() {
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "profile_observation_unavailable",
                    "The engine does not expose the required profile inventory.",
                ));
            }
            let mut bytes = Vec::with_capacity(2048);
            while let Some(chunk) = response.chunk().await.map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "profile_observation_unavailable",
                    "The engine profile observation was interrupted.",
                )
            })? {
                if chunk.len() > 1048576usize.saturating_sub(bytes.len()) {
                    return Err(ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "profile_observation_limit",
                        "The engine profile observation exceeded one MiB.",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, ApiError>(bytes)
        };
        let (props, slots) = tokio::try_join!(read("/props"), read("/slots"))?;
        let invalid = |error: crate::profiles::Error| {
            ApiError::new(StatusCode::BAD_GATEWAY, error.code, error.message)
        };
        let props = crate::profiles::parse_props(&props).map_err(invalid)?;
        let slots = crate::profiles::parse_slots(&slots).map_err(invalid)?;
        props.validate_slots(&slots).map_err(invalid)?;
        Ok((props, slots))
    }

    pub async fn context(&self, id: &str) -> Result<CacheReply<serde_json::Value>, ApiError> {
        if self.memory_protected() {
            return self.protected_context(id).await;
        }
        let activity = self
            .cache_activity(false, crate::cache::memory::Work::Bounded)
            .await?;
        let file_activity = activity.clone();
        let worker = self.worker(id)?;
        let file = worker.spec.model_file.clone();
        let declared = tokio::task::spawn_blocking(move || {
            let _activity = file_activity;
            std::fs::File::open(file)
                .map_err(|_| ())
                .and_then(|mut file| crate::profiles::read_gguf(&mut file).map_err(|_| ()))
        })
        .await
        .ok()
        .and_then(Result::ok);
        let state = worker.state.lock().await;
        self.ensure_registered(&worker)?;
        let launch = worker
            .spec
            .execution_profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?
            .1;
        let applied = state.running.as_ref().map(|run| serde_json::json!({"source":"spawned argument vector","launch":run.plan.effective(),"launch_plan":run.plan.summary(),"pid":run.child.id(),"execution_fingerprint":run.fingerprint,"runtime_identity":run.runtime_summary.lock().unwrap_or_else(|error| error.into_inner()).clone(),"engine_confirmed":false}));
        let (observed, observation_error) = if let Some(run) = &state.running {
            if worker.spec.execution_profile.adapter == crate::config::Provider::LlamaCpp {
                match self.profile_observation(&run.base).await {
                    Ok((props, slots)) => {
                        (Some(serde_json::json!({"props":props,"slots":slots})), None)
                    }
                    Err(error) => (None, Some(error.code)),
                }
            } else {
                (None, Some("provider_observation_unsupported"))
            }
        } else {
            (None, Some("worker_unloaded"))
        };
        Ok(CacheReply::new(
            serde_json::json!({"schema_version":1,"model":id,"revision":state.revision,"provider":worker.spec.execution_profile.adapter,"declared":declared,"configured":launch,"applied":applied,"observed":observed,"observation_error":observation_error,"qualification":"unqualified","note":"Training context is model metadata. Launch context is a total budget; only engine observations describe serving slots. No context is inferred by division."}),
            activity,
        ))
    }

    async fn check_config_size(next: Config) -> Result<Config, ApiError> {
        tokio::task::spawn_blocking(move || {
            next.check_persisted_size()
                .map_err(configuration_size_error)?;
            Ok(next)
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_validation_interrupted",
                "Configuration validation was interrupted. Its saved state is unchanged.",
            )
        })?
    }

    async fn write_config_io(
        &self,
        next: Config,
    ) -> Result<(Config, bool), publication::WriteError> {
        let path = self.config_path.clone();
        let lease = self.config_root.clone();
        let scope = self.live_roots.clone();
        #[cfg(test)]
        let probe = self.publication_probe.lock().unwrap().clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(probe) = &probe { publication::Probe::checkpoint(probe.clone()); }
            next.check_persisted_size().map_err(|error| publication::WriteError::Rejected(configuration_size_error(error)))?;
            let saved = match lease {
                Some(root) => scope.expect("managed configuration storage scope").write_json(&root, path.file_name().expect("configuration file name"), &next),
                None => next.persist(&path),
            };
            #[cfg(test)]
            if saved.is_ok() && probe.as_ref().is_some_and(|probe| probe.panic_after_commit()) {
                panic!("controlled configuration publisher failure after a real commit");
            }
            #[cfg(test)]
            let warning = probe.as_ref().is_some_and(|probe| probe.warning());
            #[cfg(not(test))]
            let warning = false;
            match saved {
                Ok(_) => Ok((next, warning)),
                Err(error) if error.committed() => Ok((next, true)),
                Err(error) if error.root_unavailable() => Err(publication::WriteError::Rejected(storage_root_error())),
                Err(_) => Err(publication::WriteError::Rejected(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "policy_save", "Configuration could not be saved. Check directory ownership and permissions."))),
            }
        }).await.map_err(|_| publication::WriteError::Uncertain(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "configuration_publication_uncertain", "Configuration publication has an uncertain outcome. New work is paused; restart from the preserved configuration before retrying.")))?
    }

    pub async fn register(self: &Arc<Self>, mut spec: ModelSpec) -> Result<ModelView, ApiError> {
        if spec.policy != Policy::Disabled
            || spec.revision != 0
            || spec.previous_artifact_set.is_some()
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "registration_policy",
                "New registrations must start Disabled at revision zero without rollback history.",
            ));
        }
        let artifact = if let Some(id) = &spec.artifact {
            let store = self.artifacts()?;
            let id = id.clone();
            let set = spec.artifact_set.clone();
            let companions = spec.execution_profile.adapter.accepts_companions();
            let held = tokio::task::spawn_blocking(move || {
                store.hold_binding_with(&id, set.as_deref(), companions)
            })
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "artifact_inspection_interrupted",
                    "Artifact validation was interrupted.",
                )
            })?
            .map_err(ApiError::from)?;
            spec.model_file = held.path().to_owned();
            Some(held)
        } else {
            None
        };
        if spec.execution_profile.adapter.accepts_companions() || !spec.companions.is_empty() {
            let (template, _) = spec
                .execution_profile
                .resolved_arguments()
                .map_err(|error| {
                    ApiError::new(StatusCode::BAD_REQUEST, error.code, error.message)
                })?;
            let companions = match artifact.as_ref() {
                Some(held) => held.companions(),
                None => spec
                    .companions
                    .iter()
                    .map(|companion| {
                        (
                            companion.name.as_str(),
                            companion.role,
                            companion.path.as_path(),
                        )
                    })
                    .collect(),
            };
            template
                .verify_members(companions.iter().map(|(name, role, _)| (*name, *role)))
                .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?;
        }
        let config = self.config.clone().lock_owned().await;
        if config.models.iter().any(|model| model.id == spec.id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_exists",
                "This model ID is already registered.",
            ));
        }
        if config.aliases.entries.contains_key(&spec.id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "alias_conflict",
                "This model ID is saved as an alias. Delete or rename the alias before registering it.",
            ));
        }
        let mut next = config.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration generation cannot advance.",
            )
        })?;
        spec.revision = next.generation;
        next.models.push(spec.clone());
        next.validate().map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_registration", "The model registration has invalid identity, launch, reservation or operation settings."))?;
        let next = Self::check_config_size(next).await?;
        let resolved = next
            .models
            .last()
            .expect("new model")
            .resolved(self.config_path.parent().expect("configuration parent"));
        let config = if let Some(held) = &artifact {
            let store = self.artifacts()?;
            let id = held.artifact().id.clone();
            let set = held.set_id().map(str::to_owned);
            let reference = format!("model:{}", spec.id);
            tokio::task::spawn_blocking(move || {
                store.reference_binding(&id, set.as_deref(), &reference, true, store.generation())
                    .map_err(ApiError::from)?;
                Ok::<_, ApiError>(config)
            }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR,"artifact_reference_interrupted","Artifact references were interrupted. Inspect the preserved registry and storage before retrying."))??
        } else {
            config
        };
        let resolved = self.bind_runtime(resolved).await?;
        let worker = Self::create_worker(
            resolved,
            artifact.map(Arc::new),
            &self.admission,
            &self.processes,
            None,
        )?;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::Insert { worker })
            .await?;
        let warning = outcome.warning;
        let view = outcome.model.expect("inserted model observation");
        if warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Registration committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        Ok(view)
    }

    pub async fn unregister(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
    ) -> Result<serde_json::Value, ApiError> {
        let worker = self.worker(id)?;
        let state = worker.state.clone().lock_owned().await;
        if state.revision != expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The model changed. Refresh before removing its registration.",
            ));
        }
        if state.policy != Policy::Disabled
            || worker.preparing.load(Ordering::Acquire)
            || state.running.is_some()
            || worker.active.load(Ordering::Acquire) != 0
            || self.processes.snapshot(id).is_some()
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_in_use",
                "Disable and unload the model before removing its registration.",
            ));
        }
        let config = self.config.clone().lock_owned().await;
        let dependents: Vec<&str> = config
            .aliases
            .entries
            .iter()
            .filter(|(_, target)| *target == id)
            .map(|(alias, _)| alias.as_str())
            .take(8)
            .collect();
        if !dependents.is_empty() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "alias_target",
                format!(
                    "Re-point or delete the aliases that target this model before removing it: {}.",
                    dependents.join(", ")
                ),
            ));
        }
        let mut next = config.clone();
        self.ensure_registered(&worker)?;
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration generation cannot advance.",
            )
        })?;
        next.models.retain(|model| model.id != id);
        let outcome = self
            .publish_configuration(
                config,
                next,
                publication::Effect::Remove {
                    worker: worker.clone(),
                    state,
                },
            )
            .await?;
        let warning = outcome.warning;
        if warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Removal committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        let reference_retained = if worker.spec.artifact.is_some() {
            let store = self.artifacts()?;
            let reference = format!("model:{id}");
            let configuration = self.config.clone().lock_owned().await;
            if configuration.models.iter().any(|model| model.id == id) {
                true
            } else {
                tokio::task::spawn_blocking(move || {
                    let _configuration = configuration;
                    let artifact = worker.spec.artifact.as_deref().expect("managed artifact");
                    if worker.spec.previous_artifact_set.is_some() {
                        return store
                            .reference_bindings(&reference, &[], store.generation())
                            .is_err();
                    }
                    store
                        .reference_binding(
                            artifact,
                            worker.spec.artifact_set.as_deref(),
                            &reference,
                            false,
                            store.generation(),
                        )
                        .is_err()
                })
                .await
                .unwrap_or(true)
            }
        } else {
            false
        };
        let sessions_forgotten = self
            .forget_session_rows(Some(crate::sessions::model_tag(id)), None)
            .await
            .map_or(0, |(forgotten, _)| forgotten);
        Ok(
            serde_json::json!({"model":id,"registration_removed":true,"weights_retained":true,"artifact_reference_retained":reference_retained,"sessions_forgotten":sessions_forgotten}),
        )
    }

    fn ensure_registered(&self, worker: &Arc<Worker>) -> Result<(), ApiError> {
        if !self
            .workers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&worker.spec.id)
            .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_replaced",
                "This registration was removed or replaced. Refresh before retrying.",
            ));
        }
        Ok(())
    }

    fn cache_store(&self) -> Result<Arc<CacheStore>, ApiError> {
        self.cache.clone().ok_or_else(|| {
            if let Some(code) = self.cache_memory_unavailable {
                return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, code, "Protected capacity could not admit cache metadata. The cache remains unopened. Restore capacity and restart the service.");
            }
            ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "cache_storage_unavailable",
                "Managed cache storage is unavailable.",
            )
        })
    }

    pub async fn cache_view(
        &self,
        context: &RequestContext,
        request: crate::cache::PageRequest,
    ) -> Result<CacheReply<CacheView>, ApiError> {
        let config = self.config.lock().await;
        let enabled = config.cache_enabled;
        let configuration_generation = config.generation;
        drop(config);
        let cache = self.cache_store()?;
        let context = context.clone();
        let activity = self
            .cache_activity(
                false,
                if request.limit() <= crate::cache::DEFAULT_PAGE_SIZE {
                    crate::cache::memory::Work::Page
                } else {
                    crate::cache::memory::Work::Metadata
                },
            )
            .await?;
        tokio::task::spawn_blocking(move || {
            let _activity = activity.clone();
            let (generation, data, page) = cache
                .inventory_page(context.principal, &context.credential_epoch, &request)
                .map_err(cache_error)?;
            Ok(CacheReply::new(
                CacheView {
                    schema_version: 1,
                    enabled,
                    configuration_generation,
                    generation,
                    data,
                    page,
                    qualification: "unqualified",
                    scope: "explicit native slot snapshots",
                },
                activity,
            ))
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_observation_interrupted",
                "The snapshot inventory observation was interrupted.",
            )
        })?
    }

    pub async fn configure_cache(
        self: &Arc<Self>,
        enabled: bool,
        expected_generation: u64,
    ) -> Result<(), ApiError> {
        self.cache_store()?;
        let config = self.config.clone().lock_owned().await;
        if config.generation != expected_generation {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Configuration changed. Refresh before changing cache policy.",
            ));
        }
        if self.cache_operations.policy_bound() != 0 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "cache_busy",
                "Finish active snapshot operations before changing cache policy.",
            ));
        }
        let mut next = config.clone();
        next.cache_enabled = enabled;
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "Configuration generation cannot advance.",
            )
        })?;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        let warning = outcome.warning;
        if warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Cache policy committed but directory durability is unconfirmed.",
            ));
        }
        Ok(())
    }

    fn cache_session(session: &str) -> Result<(), ApiError> {
        if session.is_empty() || session.len() > 512 || session.chars().any(char::is_control) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_session",
                "Provide a bounded opaque session identifier.",
            ));
        }
        Ok(())
    }

    fn cache_eligible(worker: &Worker, slot: u32, session: &str) -> Result<(), ApiError> {
        Self::cache_session(session)?;
        let binding = worker
            .spec
            .execution_profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?
            .0
            .has_cache_binding()
            || worker.spec.execution_profile.adapter == crate::config::Provider::LlamaCpp;
        if worker.spec.max_inflight != 1 || slot != 0 || !binding {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "cache_profile_unsupported",
                "This profile must declare one actual slot and an engine slot-save path using {cache}. Multi-slot cache ownership is not qualified.",
            ));
        }
        Ok(())
    }

    pub async fn forget_snapshot(
        &self,
        request: CacheForget,
        context: RequestContext,
    ) -> Result<CacheReply<serde_json::Value>, ApiError> {
        Self::cache_session(&request.session)?;
        let cache = self.cache_store()?;
        let activity = self
            .cache_activity(false, crate::cache::memory::Work::Bounded)
            .await?;
        let snapshot_id = request.snapshot.clone();
        let work_activity = activity.clone();
        tokio::task::spawn_blocking(move || {
            let _activity = work_activity;
            let snapshot =
                cache.observation(&snapshot_id, context.principal, &context.credential_epoch)?;
            let identity = snapshot_identity(
                &snapshot.execution_fingerprint,
                &context,
                &request.session,
                &snapshot.protocol,
            );
            cache.forget(&snapshot_id, &identity, request.expected_revision)
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_forget_interrupted",
                "Snapshot deletion was interrupted. Inspect retained state before retrying.",
            )
        })?
        .map_err(cache_error)?;
        Ok(CacheReply::new(
            serde_json::json!({"forgotten":true,"snapshot":request.snapshot}),
            activity,
        ))
    }

    pub async fn inspect_cache(
        &self,
        request: crate::cache::PageRequest,
    ) -> Result<CacheReply<crate::cache::CacheInspection>, ApiError> {
        let cache = self.cache_store()?;
        let activity = self
            .cache_activity(
                false,
                if request.limit() <= crate::cache::DEFAULT_PAGE_SIZE {
                    crate::cache::memory::Work::Bounded
                } else {
                    crate::cache::memory::Work::Metadata
                },
            )
            .await?;
        tokio::task::spawn_blocking(move || {
            let _activity = activity.clone();
            cache
                .inspect_page(&request)
                .map(|view| CacheReply::new(view, activity))
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_inspection_interrupted",
                "Cache inspection was interrupted.",
            )
        })?
        .map_err(cache_error)
    }

    fn worker_snapshot(&self) -> Vec<Arc<Worker>> {
        self.workers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn worker(&self, id: &str) -> Result<Arc<Worker>, ApiError> {
        self.workers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_model",
                    "The model is not registered in this service.",
                )
            })
    }

    fn reap(state: &mut WorkerState) {
        let exited = state
            .running
            .as_mut()
            .is_some_and(|run| matches!(run.child.try_wait(), Ok(Some(_))));
        if exited {
            state.running = None;
            state.error = Some("worker_exited");
        }
    }

    fn view(worker: &Worker, state: &WorkerState) -> ModelView {
        let downloaded = worker.spec.model_file.is_file();
        let mut view = ModelView {
            process_lifecycle: None,
            cleanup_retry_available: false,
            uncertain_requests: worker.uncertain_requests.load(Ordering::Acquire),
            uncertain_state_operations: worker.uncertain_state_operations.load(Ordering::Acquire),
            request_limits: worker
                .protected_memory
                .then(|| {
                    worker
                        .spec
                        .resource_profile
                        .as_ref()
                        .map(|profile| profile.request_limits.clone())
                })
                .flatten(),
            residency: None,
            // Protected request profiles accept only text parts, so images would be refused.
            vision: worker.spec.execution_profile.binds_projector()
                && !(worker.protected_memory && worker.spec.resource_profile.is_some()),
            preparation_pending: worker.preparing.load(Ordering::Acquire),
            startup_waiting: worker.startup_waiting.load(Ordering::Acquire),
            id: worker.spec.id.clone(),
            artifact_set: worker.spec.artifact_set.clone(),
            runtime_package: worker
                .spec
                .execution_profile
                .runtime_package()
                .map(str::to_owned),
            implementation_id: worker.spec.execution_profile.adapter.implementation_id(),
            policy: state.policy,
            revision: state.revision,
            state: if state.error.is_some() {
                "failed"
            } else if state.running.is_some() {
                "ready"
            } else if !downloaded {
                "missing-weights"
            } else {
                "unloaded"
            },
            downloaded,
            loaded: state.running.is_some(),
            pid: state.running.as_ref().and_then(|run| run.child.id()),
            active_requests: worker.active.load(Ordering::Acquire),
            max_inflight: worker.spec.max_inflight,
            resident_bytes: if worker.protected_memory {
                worker
                    .spec
                    .resource_profile
                    .as_ref()
                    .map(|profile| profile.resident_bytes)
            } else {
                Some(worker.spec.resident_bytes)
            },
            operations: worker.spec.operations.clone(),
            error: state.error,
            runtime_identity: state.running.as_ref().map(|run| {
                run.runtime_summary
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
            }),
        };
        Self::overlay_process(worker, &mut view, Some(state.running.is_some()));
        view
    }

    fn cleanup_exceptions(
        worker: &Worker,
        running: bool,
        process: Option<&crate::process::Observation>,
    ) -> (bool, bool) {
        let Some(process) = process.filter(|process| {
            !running
                && matches!(
                    process.phase,
                    crate::process::Phase::StopFailed | crate::process::Phase::CleanupPending
                )
        }) else {
            return (false, false);
        };
        (
            worker.preparing.load(Ordering::Acquire),
            worker.speech_retiring.load(Ordering::Acquire) == process.generation,
        )
    }

    fn overlay_process(worker: &Worker, view: &mut ModelView, running: Option<bool>) {
        view.preparation_pending = worker.preparing.load(Ordering::Acquire);
        view.startup_waiting = worker.startup_waiting.load(Ordering::Acquire);
        view.uncertain_requests = worker.uncertain_requests.load(Ordering::Acquire);
        view.uncertain_state_operations = worker.uncertain_state_operations.load(Ordering::Acquire);
        let process = worker.processes.snapshot(&worker.spec.id);
        let (preparation_cleanup, speech_cleanup) = running.map_or((false, false), |running| {
            Self::cleanup_exceptions(worker, running, process.as_ref())
        });
        view.cleanup_retry_available = (preparation_cleanup || speech_cleanup)
            && (worker.active.load(Ordering::Acquire) == 0 || speech_cleanup);
        if view.uncertain_requests != 0 || view.uncertain_state_operations != 0 {
            view.state = "cleanup-pending";
            view.error = Some(if view.uncertain_state_operations != 0 {
                "state_termination_unconfirmed"
            } else {
                "request_termination_unconfirmed"
            });
            view.cleanup_retry_available = worker.active.load(Ordering::Acquire)
                == view.uncertain_requests + view.uncertain_state_operations
                && !worker.physical_invariant_failed.load(Ordering::Acquire);
        }
        if let Some(process) = &process {
            use crate::process::Phase;
            if matches!(
                process.phase,
                Phase::Stopping | Phase::Cleaning | Phase::StopFailed | Phase::CleanupPending
            ) {
                let failed = matches!(process.phase, Phase::StopFailed | Phase::CleanupPending);
                view.state = if failed {
                    "cleanup-pending"
                } else {
                    "stopping"
                };
                view.pid = process.pid;
                view.loaded = process.pid.is_some();
                view.error = failed.then_some("worker_cleanup_pending");
            }
        } else if matches!(view.state, "cleanup-pending" | "stopping") {
            view.state = if view.downloaded {
                "unloaded"
            } else {
                "missing-weights"
            };
            view.pid = None;
            view.loaded = false;
            view.error = None;
        }
        if view.preparation_pending && process.is_none() {
            view.state = "loading";
            view.loaded = false;
            view.pid = None;
            view.error = None;
        }
        view.process_lifecycle = process;
    }

    fn process_available(worker: &Worker, running: bool) -> Result<(), ApiError> {
        if worker.preparing.load(Ordering::Acquire) {
            return Err(preparation_pending());
        }
        let process = worker.processes.snapshot(&worker.spec.id);
        if process
            .is_some_and(|process| !running || process.phase != crate::process::Phase::Running)
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_cleanup_pending",
                "The previous worker generation has not confirmed exit. Its resources remain reserved. Retry cleanup before loading or using it.",
            ));
        }
        Ok(())
    }

    fn publish(worker: &Worker, state: &WorkerState) {
        *worker.observation.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Self::view(worker, state));
    }

    pub async fn catalog(&self) -> Vec<ModelView> {
        let mut result = Vec::new();
        for worker in self.worker_snapshot() {
            let running = if let Ok(mut state) = worker.state.try_lock() {
                Self::reap(&mut state);
                Self::publish(&worker, &state);
                Some(state.running.is_some())
            } else {
                None
            };
            let mut view = worker
                .observation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .expect("worker has an initial observation")
                .clone();
            Self::overlay_process(&worker, &mut view, running);
            view.active_requests = worker.active.load(Ordering::Acquire);
            self.residency_overlay(&mut view);
            result.push(view);
        }
        result
    }

    pub fn resources(&self) -> serde_json::Value {
        let (source, capacity, reserved) = self.admission.memory_snapshot().map_or_else(
            || {
                (
                    "operator-declared",
                    Some(self.budget.limit),
                    self.budget.used.load(Ordering::Acquire),
                )
            },
            |memory| {
                (
                    "declared-protected-host",
                    memory.capacity_bytes,
                    memory.reserved_bytes,
                )
            },
        );
        serde_json::json!({ "reservation_source": source, "resident_budget_bytes": capacity,
            "reserved_bytes": reserved, "measured_memory_bytes": null, "paused": self.paused.load(Ordering::Acquire),
            "power": self.power.snapshot(), "host_memory": self.host_memory.observation(),
            "runtime_control_memory": self.runtime_control.projection(),
            "cache_control_memory": self.cache.as_ref().and_then(|cache| cache.control_memory()).map(|memory| memory.projection()).unwrap_or_else(|| serde_json::json!({"state":if self.cache_memory_unavailable.is_some() { "unavailable" } else { "legacy" },"reason":self.cache_memory_unavailable,"metadata_bytes":0,"scratch_bytes":0,"reserved_bytes":0,"outstanding_bytes":0,"active_operations":0})),
            "active_requests": self.active_requests() })
    }

    fn active_requests(&self) -> u64 {
        self.workers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|worker| worker.active.load(Ordering::Acquire) as u64)
            .sum()
    }

    pub fn logs(&self) -> &Arc<crate::logs::LogBuffer> {
        &self.logs
    }

    pub fn metrics(&self) -> &crate::metrics::Counters {
        &self.metrics
    }

    pub fn cors(&self) -> &crate::cors::Policy {
        &self.cors
    }

    fn capture_worker_output(
        &self,
        worker: &Worker,
        child: &mut crate::process::ManagedChild<WorkerResources>,
    ) {
        use crate::logs::{Level, Origin, Stream};
        let origin = Origin::Worker {
            model: worker.record_model.clone(),
            generation: child.generation(),
        };
        let (stdout, stderr) = child.take_output();
        let captured = [
            stdout.map(|pipe| self.logs.capture(origin.clone(), Stream::Stdout, pipe)),
            stderr.map(|pipe| self.logs.capture(origin, Stream::Stderr, pipe)),
        ];
        if captured
            .iter()
            .any(|result| !matches!(result, Some(Ok(()))))
        {
            self.logs.service(
                Level::Warning,
                &format!("Worker {} output capture is incomplete", worker.spec.id),
            );
        }
        self.logs.service(
            Level::Info,
            &format!(
                "Worker {} generation {} started with pid {}",
                worker.spec.id,
                child.generation(),
                child
                    .id()
                    .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string())
            ),
        );
    }

    pub async fn system_stats(self: &Arc<Self>) -> serde_json::Value {
        let sample = self.system_stats.current().await;
        let mut stats =
            crate::system_stats::projection(sample.as_deref(), self.system_stats.interval());
        match self.hardware.latest() {
            Some(snapshot) => {
                crate::system_stats::name_gpus(&mut stats, &snapshot.report.devices.gpus)
            }
            None if self.hardware.claim_warmup() => {
                let manager = self.clone();
                tokio::spawn(async move {
                    let _ = manager.hardware.snapshot().await;
                    manager.hardware.release_warmup();
                });
            }
            None => {}
        }
        stats["active_requests"] = self.active_requests().into();
        stats["queued_requests"] = (self.admission.pending_count() as u64).into();
        stats["last_inference"] = serde_json::to_value(
            self.last_inference
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        )
        .unwrap_or_default();
        stats
    }

    pub fn admission(&self) -> serde_json::Value {
        serde_json::json!({"schema_version":1,"paused":self.paused.load(Ordering::Acquire),"shutting_down":self.shutting_down.load(Ordering::Acquire),"resources":self.resources(),"policies_preserved":true,"scheduler":self.admission.snapshot(),"processes":self.processes.snapshots(),"pending_preparations":self.pending_preparations(),"startup":self.startup.snapshot(),"preparation_scope":"per-worker ownership with a separate global startup permit","ingress":self.ingress.view(),"slot_capacity_source":"operator-declared; engine observation remains unqualified"})
    }

    fn pending_preparations(&self) -> Vec<String> {
        self.worker_snapshot()
            .into_iter()
            .filter(|worker| worker.preparing.load(Ordering::Acquire))
            .map(|worker| worker.spec.id.clone())
            .collect()
    }

    pub(crate) fn reserve_ingress(
        &self,
        bytes: usize,
    ) -> Result<crate::admission::IngressLease, ApiError> {
        self.ingress.try_reserve(bytes).map_err(ApiError::from)
    }

    pub fn pause(&self, paused: bool) -> Result<serde_json::Value, ApiError> {
        if !paused && self.publications.is_uncertain() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_publication_uncertain",
                "Configuration publication has an uncertain outcome. Preserve storage and restart before resuming new work.",
            ));
        }
        if !paused && self.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The service is shutting down and cannot resume new work.",
            ));
        }
        self.paused.store(paused, Ordering::Release);
        self.startup.pause(paused);
        self.admission.pause(paused);
        Ok(self.admission())
    }

    pub fn suitability(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        let worker = self
            .workers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unknown_model",
                    "Choose a registered model for suitability inspection.",
                )
            })?;
        let observation = worker.observation.lock().unwrap_or_else(|e| e.into_inner());
        let model = observation
            .as_ref()
            .expect("worker has an initial observation");
        let host = self.protected_suitability(&worker);
        let (source, capacity, used, available, additional, fits) = if let Some(host) = &host {
            (
                "declared-protected-host",
                host.snapshot.capacity_bytes,
                host.snapshot.reserved_bytes,
                host.available,
                host.additional,
                host.fits,
            )
        } else {
            let used = self.budget.used.load(Ordering::Acquire);
            // A loaded model already holds its reservation; judge it as a replacement.
            let others = if model.loaded {
                used.saturating_sub(worker.spec.resident_bytes)
            } else {
                used
            };
            (
                "operator-declared",
                Some(self.budget.limit),
                used,
                Some(self.budget.limit.saturating_sub(others)),
                Some(worker.spec.resident_bytes),
                Some(
                    self.budget
                        .after_reservation(others, worker.spec.resident_bytes)
                        .is_some(),
                ),
            )
        };
        let shutting_down = self.shutting_down.load(Ordering::Acquire);
        let paused = self.paused.load(Ordering::Acquire);
        let mut blockers = Vec::new();
        if model.policy == Policy::Disabled {
            blockers.push("model_disabled");
        }
        if !model.downloaded {
            blockers.push("missing_weights");
        }
        if worker.uncertain_requests.load(Ordering::Acquire) != 0 {
            blockers.push("request_termination_unconfirmed");
        }
        if worker.uncertain_state_operations.load(Ordering::Acquire) != 0 {
            blockers.push("state_termination_unconfirmed");
        }
        if model.state == "loading" {
            blockers.push("worker_transition");
        }
        if shutting_down {
            blockers.push("shutting_down");
        }
        if paused {
            blockers.push("admission_paused");
        }
        if let Some(host) = &host {
            if let Some(blocker) = host.blocker {
                blockers.push(blocker);
            }
        } else if fits == Some(false) {
            blockers.push("resident_budget");
        }
        Ok(serde_json::json!({
            "schema_version": 1, "contract_scope": if host.is_some() {"declared-protected-host"} else {"declared-reservation-only"},
            "model": model.id, "policy": model.policy, "policy_revision": model.revision,
            "profile": null, "recipe": null, "context_tokens": null,
            "downloaded": model.downloaded, "enabled": model.policy != Policy::Disabled,
            "compatibility": {"compatible": null, "reason": if host.is_some() {"A complete physical resource qualification is unavailable."} else {"Artifact and execution-profile identity have not migrated."}},
            "artifact": null, "read_only": true, "reservation": false,
            "observation_source": "last-published-worker-observation",
            "resources": {
                "schema_version": 1, "fit": "unknown", "reason_code": if host.is_some() {"resource_qualification_unavailable"} else {"resource_identity_unavailable"},
                "reason": if host.is_some() {"Declared ceilings and a point observation do not qualify model fit, interference or hardware behavior."} else {"Physical capacity, saved context, workspace ceilings and complete execution identity are unavailable. A declared reservation is not hardware fit."},
                "hardware_qualified": false, "recommendation_qualified": false,
                "admission_open": !shutting_down && !paused, "blockers": blockers,
                "assessment_scope": if model.loaded && host.is_none() {"replace-running-worker"} else {"additional-worker"}, "reservation_source": source,
                "declared_budget": {
                    "capacity_bytes": capacity, "reserved_bytes": used,
                    "available_bytes": available,
                    "additional_worker_bytes": additional,
                    "fits_additional_reservation": fits
                },
                "measured_memory_bytes": null
            }
        }))
    }

    pub fn records(&self) -> Vec<RequestRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub async fn set_policy(
        self: &Arc<Self>,
        id: &str,
        policy: Policy,
        expected_revision: u64,
    ) -> Result<ModelView, ApiError> {
        let worker = self.worker(id)?;
        if policy == Policy::Disabled {
            self.flush_sessions(&worker).await;
        }
        let mut state = worker.state.clone().lock_owned().await;
        Self::reap(&mut state);
        if state.revision != expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The policy changed in another client. Refresh and review the current policy before saving again.",
            ));
        }
        let stop_after = policy == Policy::Disabled
            && (state.running.is_some() || self.processes.snapshot(id).is_some());
        if policy == Policy::Disabled && worker.active.load(Ordering::Acquire) > 0 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_busy",
                "Requests are still running on this model. Let them finish, then disable it.",
            ));
        }
        if policy == Policy::Disabled
            && worker.preparing.load(Ordering::Acquire)
            && !worker.startup_waiting.load(Ordering::Acquire)
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_starting",
                "This model is still starting. Disable it once it has loaded.",
            ));
        }
        let config = self.config.clone().lock_owned().await;
        self.ensure_registered(&worker)?;
        let mut next = config.clone();
        let revision = next
            .generation
            .max(state.revision)
            .checked_add(1)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "revision_exhausted",
                    "The configuration generation cannot advance.",
                )
            })?;
        next.generation = revision;
        let entry = next
            .models
            .iter_mut()
            .find(|entry| entry.id == id)
            .expect("registered model has config");
        entry.policy = policy;
        entry.revision = revision;
        let outcome = self
            .publish_configuration(
                config,
                next,
                publication::Effect::Policy {
                    worker,
                    state,
                    policy,
                    revision,
                },
            )
            .await?;
        let durability_unknown = outcome.warning;
        if durability_unknown {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "policy_durability",
                "The policy was committed, but directory durability is unconfirmed. Refresh its current revision and check storage health.",
            ));
        }
        if policy == Policy::OnDemand {
            self.residency.touch(id);
        }
        if stop_after {
            return self.unload(id).await;
        }
        Ok(outcome.model.expect("published model policy"))
    }

    async fn ensure_loaded<'a>(
        &self,
        worker: &'a Arc<Worker>,
        mut state: MutexGuard<'a, WorkerState>,
        expected_revision: Option<u64>,
    ) -> Result<MutexGuard<'a, WorkerState>, ApiError> {
        if self.native_import_only {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "offline_native_import",
                "This owner is preparing an offline native import and cannot start workers.",
            ));
        }
        loop {
            self.check_startup_intent(worker, &state, expected_revision)?;
            Self::reap(&mut state);
            if worker.preparing.load(Ordering::Acquire) {
                let waiting = worker
                    .attempt
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .upgrade()
                    .map(|attempt| (attempt.completed.subscribe(), attempt.deadline));
                if let Some((mut completed, deadline)) = waiting {
                    let interruption = self.startup.interruption();
                    drop(state);
                    let result = tokio::time::timeout_at(deadline, async {
                        loop {
                            if let Some(result) = completed.borrow().clone() {
                                return result;
                            }
                            tokio::select! {
                                changed = completed.changed() => {
                                    if changed.is_err() {
                                        return Err(LoadFailure {
                                            status: StatusCode::CONFLICT,
                                            code: "worker_preparation_cancelled",
                                            message: "The previous worker startup was cancelled.".into(),
                                        });
                                    }
                                }
                                error = interruption.wait() => {
                                    return Err(LoadFailure {
                                        status: StatusCode::from_u16(error.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                                        code: error.code,
                                        message: error.message.into(),
                                    });
                                }
                            }
                        }
                    })
                    .await
                    .map_err(|_| startup_completion_timeout())?;
                    result
                        .map_err(|error| ApiError::new(error.status, error.code, error.message))?;
                    state = tokio::time::timeout_at(deadline, worker.state.lock())
                        .await
                        .map_err(|_| startup_completion_timeout())?;
                    continue;
                }
                return Err(preparation_pending());
            }
            Self::process_available(worker, state.running.is_some())?;
            if state.running.is_some() {
                self.verify_storage().await?;
                return Ok(state);
            }
            let now = tokio::time::Instant::now();
            let wait = self.startup_wait;
            let deadline = now
                .checked_add(wait)
                .and_then(|deadline| deadline.checked_add(self.startup_timeout))
                .and_then(|deadline| deadline.checked_add(crate::process::DEFAULT_STOP_TIMEOUT))
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_startup_deadline",
                        "The configured startup completion bound cannot be represented.",
                    )
                })?;
            let attempt = PreparationFlag::begin(worker, deadline)?;
            let revision = state.revision;
            Self::publish(worker, &state);
            let mut invalidated = attempt.completed.subscribe();
            drop(state);
            let acquired = tokio::select! {
                acquired = self.startup.acquire(worker.record_model.clone(), now + wait) => acquired,
                _ = invalidated.changed() => {
                    let failure = match invalidated.borrow().clone() {
                        Some(Err(failure)) => failure,
                        _ => LoadFailure {
                            status: StatusCode::CONFLICT,
                            code: "worker_preparation_cancelled",
                            message: "The previous worker startup was cancelled.".into(),
                        },
                    };
                    return Err(ApiError::new(failure.status, failure.code, failure.message));
                }
            };
            drop(invalidated);
            let permit = match acquired {
                Ok(permit) => permit,
                Err(error) => {
                    let error = startup_error(error);
                    attempt.finish(Err(LoadFailure::from(&error)));
                    return Err(error);
                }
            };
            state = match tokio::time::timeout_at(deadline, worker.state.lock()).await {
                Ok(state) => state,
                Err(_) => {
                    let error = startup_completion_timeout();
                    attempt.finish(Err(LoadFailure::from(&error)));
                    return Err(error);
                }
            };
            let result = async {
                self.check_startup_intent(worker, &state, Some(revision))?;
                if tokio::time::Instant::now() >= deadline {
                    return Err(startup_completion_timeout());
                }
                Self::reap(&mut state);
                if state.running.is_some() || worker.processes.snapshot(&worker.spec.id).is_some() {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "worker_cleanup_pending",
                        "The previous worker generation must settle before this startup can dispatch.",
                    ));
                }
                self.verify_storage().await?;
                let host = self
                    .reserve_with_eviction(
                        worker,
                        |error| error.code == "host_memory_exhausted",
                        || self.reserve_load_memory(worker),
                    )
                    .await?;
                let bytes = if self.memory_protected() { 0 } else { worker.spec.resident_bytes };
                let reservation = self
                    .reserve_with_eviction(
                        worker,
                        |error| error.code == "resident_budget",
                        || std::future::ready(self.budget.reserve(bytes)),
                    )
                    .await?;
                let preparation = Preparation::begin(attempt.clone(), reservation, permit);
                *preparation.resources.host.lock().unwrap_or_else(|error| error.into_inner()) = host;
                let dispatch_deadline = Instant::now()
                    .checked_add(self.startup_timeout)
                    .unwrap_or_else(|| deadline.into_std())
                    .min(deadline.into_std());
                self.start_prepared(worker, &mut state, preparation, dispatch_deadline).await
            }.await;
            attempt.finish(result.as_ref().map(|_| ()).map_err(LoadFailure::from));
            if let Err(error) = &result
                && state.revision == revision
            {
                state.error = Some(error.code);
            }
            drop(attempt);
            Self::publish(worker, &state);
            result?;
            return Ok(state);
        }
    }

    fn check_startup_intent(
        &self,
        worker: &Arc<Worker>,
        state: &WorkerState,
        expected_revision: Option<u64>,
    ) -> Result<(), ApiError> {
        self.ensure_registered(worker)?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The service is shutting down.",
            ));
        }
        if self.paused.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admission_paused",
                "New work is paused for maintenance. Saved model policies are unchanged.",
            ));
        }
        if expected_revision.is_some_and(|revision| revision != state.revision) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_changed",
                "The model changed while waiting. Refresh and retry.",
            ));
        }
        if state.policy == Policy::Disabled {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_disabled",
                "Enable this model's loading policy before loading or testing it.",
            ));
        }
        Ok(())
    }

    async fn start_prepared(
        &self,
        worker: &Worker,
        state: &mut WorkerState,
        preparation: Preparation,
        deadline: Instant,
    ) -> Result<(), ApiError> {
        self.verify_storage().await?;
        if !worker.spec.model_file.is_file() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "missing_weights",
                "The configured model file is unavailable. Check its path on the service host.",
            ));
        }
        let mut preparing = Self::view(worker, state);
        preparing.state = "loading";
        preparing.pid = None;
        preparing.error = None;
        *worker
            .observation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(preparing);
        let power = self.power.activity(deadline).await;
        preparation
            .resources
            .claims
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .power = Some(power);
        let listener =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|_| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "worker_port",
                    "A private worker port could not be allocated.",
                )
            })?;
        let port = listener
            .local_addr()
            .map_err(|_| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "worker_port",
                    "The worker address is unavailable.",
                )
            })?
            .port();
        let spec = worker.spec.clone();
        let artifact = worker._artifact.clone();
        let config_root = self.config_root.clone();
        let live_roots = self.live_roots.clone();
        let cache_directory = self.cache_directory.clone();
        let (plan, preparation) = blocking_preparation(
            preparation,
            move || {
                if let Some(scope) = &live_roots {
                    scope.verify().map_err(|_| storage_root_error())?;
                }
                let profile = &spec.execution_profile;
                let bundle = profile
                    .runtime_bundle()
                    .map(|bundle| bundle.verify(deadline))
                    .transpose()
                    .map_err(preparation_identity_error)?
                    .map(Arc::new);
                let model = crate::identity::VerifiedFile::open(&spec.model_file, deadline)
                    .map_err(preparation_identity_error)?;
                let executable_path = crate::identity::executable_path(&profile.command)
                    .map_err(preparation_identity_error)?;
                let executable = crate::identity::VerifiedFile::open(&executable_path, deadline)
                    .map_err(preparation_identity_error)?;
                let environment = crate::profiles::CapturedEnvironment::capture(bundle.as_deref())
                    .map_err(preparation_profile_error)?;
                let configuration = environment
                    .configuration_inputs(profile, deadline)
                    .map_err(preparation_configuration_error)?;
                let workspace = match profile.adapter {
                    crate::config::Provider::WhisperCpp => Some(
                        tempfile::Builder::new()
                            .prefix("juiced-whisper-worker-")
                            .tempdir()
                            .map_err(preparation_identity_error)?,
                    ),
                    crate::config::Provider::Kokoros => Some(
                        crate::profiles::phoneme_workspace(bundle.as_deref())
                            .map_err(preparation_identity_error)?,
                    ),
                    crate::config::Provider::Moonshine => Some(
                        tempfile::Builder::new()
                            .prefix("juiced-moonshine-")
                            .tempdir()
                            .map_err(preparation_identity_error)?,
                    ),
                    _ => None,
                };
                let public_directory = workspace
                    .as_ref()
                    .filter(|_| profile.adapter == crate::config::Provider::WhisperCpp)
                    .map(|workspace| {
                        let directory = workspace.path().join("empty-public");
                        std::fs::create_dir(&directory)?;
                        Ok::<_, std::io::Error>(directory)
                    })
                    .transpose()
                    .map_err(preparation_identity_error)?;
                crate::profiles::LaunchPlan::resolve(
                    &spec,
                    crate::profiles::VerifiedLaunchBindings {
                        model,
                        executable,
                        bundle,
                        artifact,
                        config_root,
                        environment,
                        port,
                        cache_directory,
                        workspace,
                        public_directory,
                        configuration,
                    },
                )
                .map(Arc::new)
                .map_err(preparation_profile_error)
            },
            "worker_identity",
        )
        .await?;
        let plan = plan?;
        self.verify_storage().await?;
        if let Some(profile) = self.protected_profile(worker)? {
            profile
                .verify_fingerprint(plan.fingerprint())
                .map_err(host::profile_error)?;
        }
        let fingerprint = plan.fingerprint().clone();
        let bundle = plan.bundle().cloned();
        let mut command = plan
            .command()
            .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, error.code, error.message))?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "linux")]
        {
            let parent = unsafe { libc::getpid() };
            // The signal and parent check close the fork/exec orphan window.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                    }
                    Ok(())
                });
            }
        }
        if self.shutting_down.load(Ordering::Acquire) || self.paused.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admission_closed",
                "Admission closed before this worker could start. Its saved policy is unchanged.",
            ));
        }
        let resources = WorkerResources {
            startup: preparation.resources.clone(),
            plan: plan.clone(),
            _cache: self.cache.clone(),
            speech: (worker
                .spec
                .execution_profile
                .adapter
                .media_operation()
                .is_some())
            .then(Arc::default),
            state: (plan.has_cache_binding()
                || (worker.spec.max_inflight == 1
                    && worker.spec.execution_profile.adapter == crate::config::Provider::LlamaCpp))
                .then(|| Arc::new(state::RetainedStates::default())),
            writer_model: self.cache.as_ref().map(|_| worker.record_model.clone()),
            writer_generation: AtomicU64::new(0),
            uncertain: self
                .memory_protected()
                .then(|| Arc::new(physical::UncertainClaims::new(worker.spec.max_inflight))),
        };
        drop(listener);
        let mut child = self
            .processes
            .spawn(worker.record_model.clone(), &mut command, resources)
            .map_err(|error| {
                state.error = Some(error.code);
                self.logs.service(
                    crate::logs::Level::Error,
                    &format!("Worker {} could not start: {}", worker.spec.id, error.code),
                );
                ApiError::new(
                    StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY),
                    error.code,
                    error.message,
                )
            })?;
        self.capture_worker_output(worker, &mut child);
        child
            .resources()
            .writer_generation
            .store(child.generation(), Ordering::Release);
        let base = format!("http://127.0.0.1:{port}");
        let mut loading = Self::view(worker, state);
        loading.state = "loading";
        loading.pid = child.id();
        loading.error = None;
        *worker.observation.lock().unwrap_or_else(|e| e.into_inner()) = Some(loading);
        let readiness = worker.spec.execution_profile.adapter.readiness_path();
        let ready =
            tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
                loop {
                    if self.shutting_down.load(Ordering::Acquire) {
                        return false;
                    }
                    if !matches!(child.try_wait(), Ok(None)) {
                        return false;
                    }
                    if let Ok(response) = self
                        .client
                        .get(format!("{base}{readiness}"))
                        .timeout(Duration::from_millis(250))
                        .send()
                        .await
                        && response.status().is_success()
                    {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap_or(false);
        if !ready {
            self.logs.service(
                crate::logs::Level::Error,
                &format!(
                    "Worker {} generation {} exited or missed its readiness deadline",
                    worker.spec.id,
                    child.generation()
                ),
            );
            let _ = child.stop().await;
            state.error = Some("worker_not_ready");
            Self::publish(worker, state);
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "worker_not_ready",
                "The engine exited or missed its readiness deadline. Check the model and launch configuration, then retry.",
            ));
        }
        if let Err(error) = self.verify_storage().await {
            let _ = child.stop().await;
            state.error = Some(error.code);
            Self::publish(worker, state);
            return Err(error);
        }
        if worker.spec.execution_profile.adapter == crate::config::Provider::LlamaCpp {
            let observation = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.profile_observation(&base),
            )
            .await
            .unwrap_or_else(|_| {
                Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "worker_not_ready",
                    "The worker missed its startup deadline.",
                ))
            });
            let valid = observation.as_ref().is_ok_and(|(props, slots)| {
                props
                    .validate_capacity(slots, worker.spec.max_inflight)
                    .is_ok()
            });
            if !valid {
                let _ = child.stop().await;
                state.error = Some("worker_slots_mismatch");
                Self::publish(worker, state);
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "worker_slots_mismatch",
                    "The engine slot inventory must match the selected physical launch profile. Inspect engine options before retrying.",
                ));
            }
        }
        let pid = child.id().expect("live child");
        let command_path = plan.executable().to_owned();
        let observed_bundle = bundle.clone();
        let observed_plan = plan.clone();
        let (runtime_identity, preparation) = blocking_preparation(
            preparation,
            move || {
                observed_plan.verify_artifact_fresh(deadline)?;
                let executable = std::fs::canonicalize(&command_path).map_err(|_| {
                    crate::runtime_identity::Refusal {
                        status: crate::runtime_identity::Status::Unknown,
                        code: "runtime_executable_unknown",
                        message: "The engine executable identity could not be resolved.",
                    }
                })?;
                let observation = crate::runtime_identity::observe(
                    pid,
                    &executable,
                    deadline,
                    crate::runtime_identity::Limits::default(),
                )?;
                crate::runtime_identity::verify_loopback_listener(&observation, port, deadline)?;
                if let Some(bundle) = &observed_bundle {
                    bundle.verify_observation(&observation)?;
                    bundle.verify_fresh(deadline)?;
                }
                Ok::<_, crate::runtime_identity::Refusal>(Arc::new(observation))
            },
            "runtime_identity_interrupted",
        )
        .await?;
        if let Err(error) = self.verify_storage().await {
            let _ = child.stop().await;
            state.error = Some(error.code);
            Self::publish(worker, state);
            return Err(error);
        }
        if let Err(error) = &runtime_identity
            && fatal_runtime_refusal(error)
        {
            let _ = child.stop().await;
            state.error = Some(error.code);
            Self::publish(worker, state);
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                error.code,
                error.message,
            ));
        }
        let fingerprint = match &runtime_identity {
            Ok(observation) => {
                let mut hash = Sha256::new();
                hash.update(b"juiced-execution-runtime-v2\0");
                hash.update(fingerprint.as_bytes());
                hash.update(observation.closure_digest.as_bytes());
                Arc::from(format!("{:x}", hash.finalize()))
            }
            Err(_) => fingerprint,
        };
        let observed_at = unix_time_ms();
        let runtime_summary = match &runtime_identity {
            Ok(observation) => RuntimeSummary {
                state: "observed",
                bundle_digest: bundle
                    .as_ref()
                    .map(|bundle| Arc::from(bundle.digest.as_str())),
                qualification: observation.qualification,
                closure_digest: Some(Arc::from(observation.closure_digest.as_str())),
                process_generation: Some(observation.process_generation.clone()),
                reason: None,
                observed_at_unix_ms: observed_at,
                verified_at_unix_ms: Some(observed_at),
            },
            Err(error) => RuntimeSummary {
                state: match error.status {
                    crate::runtime_identity::Status::Unknown => "unknown",
                    crate::runtime_identity::Status::Unqualified => "unqualified",
                },
                qualification: "unqualified",
                bundle_digest: bundle
                    .as_ref()
                    .map(|bundle| Arc::from(bundle.digest.as_str())),
                closure_digest: None,
                process_generation: None,
                reason: Some(error.code),
                observed_at_unix_ms: observed_at,
                verified_at_unix_ms: None,
            },
        };
        if self.shutting_down.load(Ordering::Acquire) {
            let _ = child.stop().await;
            state.error = Some("shutting_down");
            Self::publish(worker, state);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The service began shutting down before the worker became ready.",
            ));
        }
        if let Some(profile) = self.protected_profile(worker)?
            && let Some(claim) = child
                .resources()
                .startup
                .host
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_mut()
        {
            claim
                .materialize(profile.resident_bytes)
                .map_err(ApiError::from)?;
        }
        child.resources().startup.release_startup();
        drop(preparation);
        worker.speech_retiring.store(0, Ordering::Release);
        self.logs.service(
            crate::logs::Level::Info,
            &format!(
                "Worker {} generation {} is ready",
                worker.spec.id,
                child.generation()
            ),
        );
        state.running = Some(Running {
            child,
            plan,
            base,
            fingerprint,
            runtime_identity,
            runtime_summary: Mutex::new(runtime_summary),
        });
        state.error = None;
        *worker
            .cache_identity
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.residency.touch(&worker.spec.id);
        Self::publish(worker, state);
        Ok(())
    }

    pub async fn load(&self, id: &str) -> Result<ModelView, ApiError> {
        let worker = self.worker(id)?;
        let mut state = worker.state.lock().await;
        self.ensure_registered(&worker)?;
        state = self.ensure_loaded(&worker, state, None).await?;
        let mut view = Self::view(&worker, &state);
        self.residency_overlay(&mut view);
        Ok(view)
    }

    async fn retire_speech_generation(&self, worker: &Arc<Worker>, generation: u64) {
        let mut state = worker.state.lock().await;
        if !state
            .running
            .as_ref()
            .is_some_and(|run| run.child.generation() == generation)
        {
            return;
        }
        worker.speech_retiring.store(generation, Ordering::Release);
        let mut run = state.running.take().expect("matching speech generation");
        state.error = Some("speech_transfer_uncertain");
        Self::publish(worker, &state);
        let result = run.child.stop().await;
        if result.is_ok() {
            run.child
                .resources()
                .speech
                .as_ref()
                .expect("selected speech generation")
                .release();
            worker.speech_retiring.store(0, Ordering::Release);
        } else {
            state.error = Some("worker_cleanup_pending");
        }
        // The exact child guard restores its retained resources to the process
        // registry if cleanup failed or this task is cancelled.
        drop(run);
        Self::publish(worker, &state);
    }

    pub async fn unload(&self, id: &str) -> Result<ModelView, ApiError> {
        let worker = self.worker(id)?;
        self.flush_sessions(&worker).await;
        let mut state = worker.state.lock().await;
        let prior_uncertain = worker.uncertain_requests.load(Ordering::Acquire)
            + worker.uncertain_state_operations.load(Ordering::Acquire);
        let cleanup_requested =
            prior_uncertain != 0 && worker.active.load(Ordering::Acquire) == prior_uncertain;
        Self::reap(&mut state);
        let process = worker.processes.snapshot(id);
        let (preparation_cleanup, speech_cleanup) =
            Self::cleanup_exceptions(&worker, state.running.is_some(), process.as_ref());
        if worker.preparing.load(Ordering::Acquire) && !preparation_cleanup {
            return Err(preparation_pending());
        }
        let uncertain = worker.uncertain_requests.load(Ordering::Acquire)
            + worker.uncertain_state_operations.load(Ordering::Acquire);
        let text_cleanup = cleanup_requested
            || (uncertain != 0 && worker.active.load(Ordering::Acquire) == uncertain);
        if state.running.as_ref().is_some_and(|run| {
            run.child
                .resources()
                .uncertain
                .as_ref()
                .is_some_and(|claims| claims.overflowed())
        }) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "physical_claim_invariant",
                "Physical claim ownership is uncertain. Preserve this manager and its retained resources for recovery.",
            ));
        }
        if worker.active.load(Ordering::Acquire) > 0 && !speech_cleanup && !text_cleanup {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_busy",
                "Active requests are using this worker. Stop or finish them before unloading.",
            ));
        }
        if state.policy == Policy::Hot && !preparation_cleanup && !speech_cleanup && !text_cleanup {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_hot",
                "Change the loading policy to On demand before unloading a Hot worker.",
            ));
        }
        if let Some(run) = state.running.as_mut() {
            let generation = run.child.generation();
            if let Err(error) = run.child.stop().await {
                state.error = Some("worker_cleanup_pending");
                Self::publish(&worker, &state);
                return Err(process_error(error));
            }
            state.running = None;
            self.record_unload(&worker, UnloadReason::Operator, generation);
        } else if let Err(error) = self.processes.retry_cleanup(id).await {
            Self::publish(&worker, &state);
            return Err(process_error(error));
        }
        worker.speech_retiring.store(0, Ordering::Release);
        state.error = None;
        Self::publish(&worker, &state);
        let mut view = Self::view(&worker, &state);
        self.residency_overlay(&mut view);
        Ok(view)
    }

    pub async fn begin(
        self: &Arc<Self>,
        id: &str,
        operation: Operation,
    ) -> Result<(String, RequestLease), ApiError> {
        if self.memory_protected() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "protected_envelope_required",
                "Protected inference requires its resolved bounded text envelope.",
            ));
        }
        let (worker, alias) = self.resolve_model(id)?;
        let started = Instant::now();
        let slot = worker
            .slots
            .try_acquire(crate::admission::Priority::Interactive)
            .map_err(|error| {
                if error.code == "request_slots" || error.status == 429 {
                    ApiError::new(
                        StatusCode::TOO_MANY_REQUESTS,
                        "request_slots",
                        "All declared request slots are in use.",
                    )
                } else {
                    ApiError::from(error)
                }
            })?;
        self.begin_admitted((worker, alias), operation, slot, started, None, 0)
            .await
    }

    pub async fn begin_queued(
        self: &Arc<Self>,
        id: &str,
        operation: Operation,
        priority: crate::admission::Priority,
    ) -> Result<(String, RequestLease), ApiError> {
        if self.memory_protected() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "protected_envelope_required",
                "Protected inference requires its resolved bounded text envelope.",
            ));
        }
        let (worker, alias) = self.resolve_model(id)?;
        let revision = {
            let observation = worker
                .observation
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let model = observation.as_ref().expect("worker observation");
            if model.policy == Policy::Disabled {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "model_disabled",
                    "Enable this model's loading policy before using it.",
                ));
            }
            if !worker.spec.operations.contains(&operation) {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "unsupported_operation",
                    "This model does not declare support for the requested operation.",
                ));
            }
            model.revision
        };
        let started = Instant::now();
        let slot = worker
            .slots
            .acquire(priority)
            .await
            .map_err(ApiError::from)?;
        self.begin_admitted(
            (worker, alias),
            operation,
            slot,
            started,
            Some(revision),
            started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        )
        .await
    }

    async fn begin_admitted(
        self: &Arc<Self>,
        (worker, alias): (Arc<Worker>, Option<Arc<str>>),
        operation: Operation,
        slot: crate::admission::Slot,
        started: Instant,
        expected_revision: Option<u64>,
        queue_wait_ms: u64,
    ) -> Result<(String, RequestLease), ApiError> {
        let mut state = worker.state.lock().await;
        self.ensure_registered(&worker)?;
        if expected_revision.is_some_and(|revision| revision != state.revision) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_changed",
                "The model changed while waiting. Refresh and retry.",
            ));
        }
        if !worker.spec.operations.contains(&operation) {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "unsupported_operation",
                "This model does not declare support for the requested operation.",
            ));
        }
        state = self
            .ensure_loaded(&worker, state, expected_revision)
            .await?;
        let power = self
            .power
            .activity(Instant::now() + self.request_timeout)
            .await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The service is shutting down.",
            ));
        }
        let base = state.running.as_ref().expect("loaded worker").base.clone();
        let fingerprint = state
            .running
            .as_ref()
            .expect("loaded worker")
            .fingerprint
            .clone();
        let generation = state
            .running
            .as_ref()
            .expect("loaded worker")
            .child
            .generation();
        *worker
            .cache_identity
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        worker.active.fetch_add(1, Ordering::AcqRel);
        let physical = PhysicalClaims {
            worker: Arc::downgrade(&worker),
            _slot: slot,
            _power: power,
            uncertain: false,
        };
        let (physical, speech, protected) = if operation.single_slot_media() {
            let run = state.running.as_ref().expect("loaded worker");
            let claims = run
                .child
                .resources()
                .speech
                .as_ref()
                .expect("selected speech generation")
                .clone();
            let mut retained = claims.0.lock().unwrap_or_else(|error| error.into_inner());
            assert!(
                retained.is_none(),
                "selected speech adapter has one physical slot"
            );
            *retained = Some(physical);
            drop(retained);
            let generation = run.child.generation();
            (
                None,
                Some(SpeechTransfer::new(self, &worker, generation, claims)),
                None,
            )
        } else if self.memory_protected() {
            let retained = state
                .running
                .as_ref()
                .expect("loaded worker")
                .child
                .resources()
                .uncertain
                .as_ref()
                .expect("protected worker generation owns uncertain transfers")
                .clone();
            (
                None,
                None,
                Some(physical::ProtectedTransfer::new(retained, physical)),
            )
        } else {
            (Some(physical), None, None)
        };
        let upstream_timeout = worker
            .spec
            .resource_profile
            .as_ref()
            .filter(|_| self.memory_protected())
            .map_or(self.request_timeout, |profile| {
                self.request_timeout
                    .min(Duration::from_millis(profile.completion_ms))
            });
        drop(state);
        let record_model = worker.record_model.clone();
        Ok((
            base,
            RequestLease {
                worker,
                manager: self.clone(),
                started,
                _physical: physical,
                speech,
                protected,
                upstream_timeout,
                fingerprint,
                generation,
                session: None,
                cache_identity: None,
                record: Some(RequestRecord {
                    id: self.next_request.fetch_add(1, Ordering::Relaxed),
                    model: record_model,
                    alias,
                    operation: operation.path(),
                    outcome: "cancelled",
                    status: None,
                    elapsed_ms: 0,
                    queue_wait_ms,
                    completed_at_ms: 0,
                    usage: Default::default(),
                    session_cache: None,
                }),
            },
        ))
    }

    pub async fn start_hot(&self) {
        self.reclaim_sessions().await;
        for worker in self.worker_snapshot() {
            let state = worker.state.lock().await;
            if state.policy == Policy::Hot {
                let revision = state.revision;
                if let Err(error) = self.ensure_loaded(&worker, state, Some(revision)).await {
                    let mut state = worker.state.lock().await;
                    if state.revision == revision {
                        state.error = Some(error.code);
                        Self::publish(&worker, &state);
                    }
                }
            }
        }
    }

    pub async fn shutdown(&self) -> ShutdownReport {
        for worker in self.worker_snapshot() {
            self.flush_sessions(&worker).await;
        }
        self.shutting_down.store(true, Ordering::Release);
        self.system_stats.close();
        self.publications.close();
        self.startup.shutdown();
        self.admission.shutdown();
        let deadline = tokio::time::Instant::now()
            + crate::process::DEFAULT_STOP_TIMEOUT
            + Duration::from_millis(250);
        let native_writers = async {
            let Some(native) = &self.native_settings else {
                return true;
            };
            if native.store.close().is_err() {
                return false;
            }
            loop {
                match native.store.snapshot() {
                    Ok(snapshot) if !snapshot.preparing => return true,
                    Err(_) => return false,
                    _ => {}
                }
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep_until(
                    (tokio::time::Instant::now() + Duration::from_millis(5)).min(deadline),
                )
                .await;
            }
        };
        let artifacts = async {
            if let Some(artifacts) = &self.artifacts {
                tokio::time::timeout_at(deadline, artifacts.shutdown())
                    .await
                    .is_ok()
            } else {
                true
            }
        };
        let workers = async {
            let stops = self.worker_snapshot().into_iter().map(|worker| async move {
                let mut state = worker.state.lock().await;
                if let Some(run) = state.running.as_mut() {
                    if run.child.stop().await.is_ok() {
                        state.running = None;
                        state.error = None;
                    } else {
                        state.error = Some("worker_cleanup_pending");
                    }
                } else {
                    let _ = worker.processes.retry_cleanup(&worker.spec.id).await;
                }
                Self::publish(&worker, &state);
            });
            let settled = tokio::time::timeout_at(deadline, futures_util::future::join_all(stops))
                .await
                .is_ok();
            loop {
                let pending = self.processes.snapshots();
                let preparations = self.pending_preparations();
                if (pending.is_empty() && preparations.is_empty())
                    || tokio::time::Instant::now() >= deadline
                {
                    return (settled, pending, preparations);
                }
                tokio::time::sleep_until(
                    (tokio::time::Instant::now() + Duration::from_millis(5)).min(deadline),
                )
                .await;
            }
        };
        let (
            artifact_writers_settled,
            (worker_stops_settled, pending_processes, pending_preparations),
            power_settled,
            configuration_settled,
            native_writers_settled,
            host_memory_settled,
            cache_operations_settled,
            runtime_operations_settled,
        ) = tokio::join!(
            artifacts,
            workers,
            self.power.shutdown(deadline.into_std()),
            self.publications.drain(deadline),
            native_writers,
            self.host_memory.settled(deadline),
            self.cache_operations.drain(deadline),
            self.runtime_control.drain(deadline)
        );
        ShutdownReport {
            pending_processes,
            pending_preparations,
            worker_stops_settled,
            artifact_writers_settled,
            power_settled,
            configuration_settled,
            native_writers_settled,
            host_memory_settled,
            cache_operations_settled,
            runtime_operations_settled,
            physical_claims_settled: self
                .worker_snapshot()
                .iter()
                .all(|worker| !worker.physical_invariant_failed.load(Ordering::Acquire)),
        }
    }
}
