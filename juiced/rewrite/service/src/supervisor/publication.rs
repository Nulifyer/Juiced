use super::{LoadFailure, Manager, ModelView, NativeSettingsSnapshot, Worker, WorkerState};
use crate::{
    ApiError,
    config::{Config, Policy},
};
use axum::http::StatusCode;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::{Notify, OwnedMutexGuard};

pub(super) enum WriteError {
    Rejected(ApiError),
    Uncertain(ApiError),
}

pub(super) enum Effect {
    ConfigOnly,
    Insert {
        worker: Arc<Worker>,
    },
    Replace {
        previous: Arc<Worker>,
        state: OwnedMutexGuard<WorkerState>,
        replacement: Arc<Worker>,
    },
    Remove {
        worker: Arc<Worker>,
        state: OwnedMutexGuard<WorkerState>,
    },
    Policy {
        worker: Arc<Worker>,
        state: OwnedMutexGuard<WorkerState>,
        policy: Policy,
        revision: u64,
    },
    NativeImport {
        snapshot: Arc<NativeSettingsSnapshot>,
        workers: Vec<Arc<Worker>>,
    },
    InsertMany {
        workers: Vec<Arc<Worker>>,
    },
    NativeEdit {
        snapshot: Arc<NativeSettingsSnapshot>,
        previous: Arc<Worker>,
        state: OwnedMutexGuard<WorkerState>,
        replacement: Arc<Worker>,
    },
    ReplaceMany {
        entries: Vec<(Arc<Worker>, OwnedMutexGuard<WorkerState>, Arc<Worker>)>,
    },
}

impl Effect {
    fn release(self) {
        match self {
            Self::Replace { state, .. }
            | Self::Remove { state, .. }
            | Self::Policy { state, .. }
            | Self::NativeEdit { state, .. } => drop(state),
            Self::ReplaceMany { entries } => drop(entries),
            _ => {}
        }
    }
}

pub(super) struct Outcome {
    pub(super) warning: bool,
    pub(super) model: Option<ModelView>,
    pub(super) generation: u64,
}

pub(super) struct Publications {
    active: AtomicUsize,
    closing: AtomicBool,
    uncertain: AtomicBool,
    changed: Notify,
}

impl Publications {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            changed: Notify::new(),
        })
    }

    pub(super) fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    pub(super) fn is_uncertain(&self) -> bool {
        self.uncertain.load(Ordering::SeqCst)
    }

    pub(super) fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    fn unavailable(&self) -> ApiError {
        if self.is_uncertain() {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_publication_uncertain",
                "Saved metadata publication is unconfirmed. Preserve configuration and runtime package files, then restart to read the atomic saved state before making further changes.",
            )
        } else {
            closed()
        }
    }

    pub(super) fn begin(self: &Arc<Self>) -> Result<Activity, ApiError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(self.unavailable());
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        if self.closing.load(Ordering::SeqCst) {
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.changed.notify_waiters();
            return Err(self.unavailable());
        }
        Ok(Activity {
            owner: self.clone(),
            finished: false,
            manager: None,
        })
    }

    pub(super) async fn drain(&self, deadline: tokio::time::Instant) -> bool {
        self.close();
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active() == 0 {
                return !self.uncertain.load(Ordering::SeqCst);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.active() == 0 && !self.uncertain.load(Ordering::SeqCst);
            }
        }
    }
}

pub(super) struct Activity {
    owner: Arc<Publications>,
    finished: bool,
    pub(super) manager: Option<Weak<Manager>>,
}

impl Activity {
    fn interrupt(&self) {
        self.owner.uncertain.store(true, Ordering::SeqCst);
        self.owner.close();
        if let Some(manager) = self.manager.as_ref().and_then(Weak::upgrade) {
            manager.paused.store(true, Ordering::Release);
            manager.startup.pause(true);
            manager.admission.pause(true);
        }
    }

    pub(super) fn finish(mut self) {
        self.finished = true;
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        if !self.finished {
            self.interrupt();
        }
        self.owner.active.fetch_sub(1, Ordering::SeqCst);
        self.owner.changed.notify_waiters();
    }
}

struct PendingPublication {
    guard: Option<OwnedMutexGuard<Config>>,
    effect: Option<Effect>,
    activity: Option<Activity>,
}

impl PendingPublication {
    fn finish(mut self) {
        self.effect.take().expect("publication effect").release();
        drop(self.guard.take());
        self.activity.take().expect("publication activity").finish();
    }
}

impl Drop for PendingPublication {
    fn drop(&mut self) {
        if let Some(activity) = &self.activity {
            activity.interrupt();
        }
    }
}

fn closed() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "configuration_publication_closed",
        "Configuration publication is closed. Wait for shutdown or restart before saving changes.",
    )
}

impl Manager {
    pub(super) async fn publish_configuration(
        self: &Arc<Self>,
        guard: OwnedMutexGuard<Config>,
        next: Config,
        effect: Effect,
    ) -> Result<Outcome, ApiError> {
        let mut activity = self.publications.begin()?;
        activity.manager = Some(Arc::downgrade(self));
        let manager = self.clone();
        let mut pending = PendingPublication {
            guard: Some(guard),
            effect: Some(effect),
            activity: Some(activity),
        };
        tokio::spawn(async move {
            let result = manager.write_config_io(next).await;
            let (next, warning) = match result {
                Ok(saved) => saved,
                Err(WriteError::Rejected(error)) => {
                    pending.finish();
                    return Err(error);
                }
                Err(WriteError::Uncertain(error)) => {
                    drop(pending);
                    return Err(error);
                }
            };
            let outcome = manager.apply_publication(
                pending.guard.as_mut().expect("publication configuration"),
                next,
                pending.effect.as_mut().expect("publication effect"),
                warning,
            );
            pending.finish();
            Ok(outcome)
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_publication_unsettled",
                "Configuration publication did not settle. Refresh saved state before restarting.",
            )
        })?
    }

    fn apply_publication(
        &self,
        guard: &mut OwnedMutexGuard<Config>,
        next: Config,
        effect: &mut Effect,
        warning: bool,
    ) -> Outcome {
        let model = match effect {
            Effect::Policy {
                worker,
                state,
                policy,
                revision,
            } => {
                state.policy = *policy;
                state.revision = *revision;
                if worker.startup_waiting.load(Ordering::Acquire) {
                    let attempt = worker
                        .attempt
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .upgrade();
                    if let Some(attempt) = attempt {
                        attempt.finish(Err(LoadFailure {
                            status: StatusCode::CONFLICT,
                            code: "model_changed",
                            message: "The model changed while waiting. Refresh and retry.".into(),
                        }));
                    }
                }
                Some(Self::view(worker, state))
            }
            Effect::Insert { worker } => Some(Self::view(
                worker,
                &worker.state.try_lock().expect("unpublished worker state"),
            )),
            Effect::Replace { replacement, .. } | Effect::NativeEdit { replacement, .. } => {
                Some(Self::view(
                    replacement,
                    &replacement
                        .state
                        .try_lock()
                        .expect("unpublished worker state"),
                ))
            }
            _ => None,
        };
        let generation = next.generation;
        let old_config = std::mem::replace(&mut **guard, next);
        self.refresh_aliases(&guard.aliases);
        self.residency.refresh(guard);
        let mut retired = Vec::new();
        let mut old_snapshot = None;
        {
            let mut workers = self
                .workers
                .write()
                .unwrap_or_else(|error| error.into_inner());
            match &*effect {
                Effect::Insert { worker } => {
                    retired.extend(workers.insert(worker.spec.id.clone(), worker.clone()));
                }
                Effect::Replace {
                    previous,
                    replacement,
                    ..
                }
                | Effect::NativeEdit {
                    previous,
                    replacement,
                    ..
                } => {
                    debug_assert!(
                        workers
                            .get(&previous.spec.id)
                            .is_some_and(|current| Arc::ptr_eq(current, previous))
                    );
                    retired
                        .extend(workers.insert(replacement.spec.id.clone(), replacement.clone()));
                }
                Effect::Remove { worker, .. } => {
                    retired.extend(workers.remove(&worker.spec.id));
                }
                Effect::ReplaceMany { entries } => {
                    for (previous, _, replacement) in entries {
                        debug_assert!(
                            workers
                                .get(&previous.spec.id)
                                .is_some_and(|current| Arc::ptr_eq(current, previous))
                        );
                        retired.extend(
                            workers.insert(replacement.spec.id.clone(), replacement.clone()),
                        );
                        if let Ok(state) = replacement.state.try_lock() {
                            Self::publish(replacement, &state);
                        }
                    }
                }
                Effect::NativeImport {
                    workers: imported, ..
                }
                | Effect::InsertMany { workers: imported } => {
                    for worker in imported {
                        retired.extend(workers.insert(worker.spec.id.clone(), worker.clone()));
                    }
                }
                _ => {}
            }
            if let Effect::NativeImport { snapshot, .. } | Effect::NativeEdit { snapshot, .. } =
                &effect
            {
                let native = self
                    .native_settings
                    .as_ref()
                    .expect("native publication owner");
                let mut current = native
                    .current
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                old_snapshot = current.replace(snapshot.clone());
            }
            if let Some(view) = &model {
                let worker = match &*effect {
                    Effect::Insert { worker } | Effect::Policy { worker, .. } => worker,
                    Effect::Replace { replacement, .. }
                    | Effect::NativeEdit { replacement, .. } => replacement,
                    _ => unreachable!(),
                };
                *worker
                    .observation
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(view.clone());
            }
        }
        if let Effect::Remove { worker, .. } = &*effect {
            worker.slots.close();
        }
        drop(old_snapshot);
        drop(retired);
        drop(old_config);
        Outcome {
            warning,
            model,
            generation,
        }
    }
}

#[cfg(test)]
pub(super) struct Probe {
    entered: std::sync::atomic::AtomicBool,
    entered_notify: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    gate: std::sync::Condvar,
    warning_after_commit: AtomicBool,
    panic_after_commit: AtomicBool,
}

#[cfg(test)]
impl Probe {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            entered: std::sync::atomic::AtomicBool::new(false),
            entered_notify: tokio::sync::Notify::new(),
            released: std::sync::Mutex::new(false),
            gate: std::sync::Condvar::new(),
            warning_after_commit: AtomicBool::new(false),
            panic_after_commit: AtomicBool::new(false),
        })
    }

    pub(super) fn warning(&self) -> bool {
        self.warning_after_commit.load(Ordering::Acquire)
    }

    pub(super) fn panic_after_commit(&self) -> bool {
        self.panic_after_commit.load(Ordering::Acquire)
    }

    pub(super) fn checkpoint(probe: std::sync::Arc<Self>) {
        let mut released = probe
            .released
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        probe
            .entered
            .store(true, std::sync::atomic::Ordering::Release);
        probe.entered_notify.notify_waiters();
        while !*released {
            released = probe
                .gate
                .wait(released)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    async fn wait_entered(&self) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let notified = self.entered_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.entered.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            tokio::time::timeout_at(deadline, notified)
                .await
                .expect("publication did not reach the blocking-write gate");
        }
    }

    fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.gate.notify_all();
    }
}

#[cfg(test)]
struct ReleaseProbe(std::sync::Arc<Probe>);

#[cfg(test)]
impl Drop for ReleaseProbe {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, Policy},
        supervisor::Manager,
    };
    use serde_json::json;
    use std::{fs, sync::Arc, time::Duration};

    fn fixture() -> (tempfile::TempDir, Arc<Manager>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let config: Config = serde_json::from_value(json!({
            "schema_version":1,"inhibit_suspend":false,"resident_budget_bytes":4096,
            "startup_timeout_ms":1000,"request_timeout_ms":1000,
            "models":[{"id":"one","model_file":"weights.gguf","provider":"external-http", "command":"never-started-engine",
                "args":["{model}","{port}"],"resident_bytes":1,"max_inflight":1,
                "operations":["completions"],"policy":"disabled","revision":0}]
        })).unwrap();
        config.persist(&path).unwrap();
        let manager = crate::storage::after_test_release(path.parent().unwrap(), || {
            Manager::open(path.clone())
        });
        (directory, manager)
    }

    async fn artifact_fixture() -> (tempfile::TempDir, Arc<Manager>, String, String, u64) {
        use sha2::{Digest, Sha256};
        use std::io::{Seek, SeekFrom, Write};
        let (directory, manager) = fixture();
        let store = manager.artifacts().unwrap();
        let bytes = b"model revision fixture";
        let digest = format!("{:x}", Sha256::digest(bytes));
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let job = store
            .start_import(
                file,
                crate::artifacts::ImportRequest {
                    expected_sha256: digest.clone(),
                    expected_bytes: Some(bytes.len() as u64),
                    maximum_bytes: bytes.len() as u64,
                },
                store.generation(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if store.job(&job.id).unwrap().state == crate::artifacts::JobState::Complete {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut spec:crate::artifacts::ArtifactSetSpec = serde_json::from_value(json!({"schema_version":1,
            "origin":{"importer_id":"publication-fixture","importer_version":"1","source_revision":"first"},
            "primary":"model","members":[{"name":"model","role":"model","artifact":digest,"bytes":bytes.len()}]})).unwrap();
        let operation = crate::artifacts::SetOperation::new(
            std::time::Instant::now() + Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
        );
        let first = store
            .publish_set(spec.clone(), store.generation(), &operation)
            .unwrap()
            .id;
        spec.origin.source_revision = "second".into();
        let second = store
            .publish_set(spec, store.generation(), &operation)
            .unwrap()
            .id;
        let mut model = manager.config.lock().await.models[0].clone();
        model.id = "revision-fixture".into();
        model.artifact = Some(digest);
        model.artifact_set = Some(first.clone());
        let registered = manager.register(model).await.unwrap();
        (directory, manager, first, second, registered.revision)
    }

    #[tokio::test]
    async fn aborted_artifact_revision_caller_retains_publication_and_both_complete_sets() {
        let (_directory, manager, first, second, revision) = artifact_fixture().await;
        let probe = Probe::new();
        let _release = ReleaseProbe(probe.clone());
        *manager.publication_probe.lock().unwrap() = Some(probe.clone());
        let caller_manager = manager.clone();
        let target = second.clone();
        let caller = tokio::spawn(async move {
            caller_manager
                .set_artifact_revision(
                    "revision-fixture".into(),
                    revision,
                    crate::supervisor::ArtifactRevisionChange::Promote {
                        artifact_set: target,
                    },
                )
                .await
        });
        probe.wait_entered().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(manager.config.try_lock().is_err());
        assert!(
            manager
                .worker("revision-fixture")
                .unwrap()
                .state
                .try_lock()
                .is_err()
        );
        assert!(manager.publications.active() >= 2);
        let store = manager.artifacts().unwrap();
        for set in [&first, &second] {
            assert_eq!(
                store.artifact_set(set).unwrap().references,
                ["model:revision-fixture"]
            );
            assert!(matches!(
                store.remove_set(set, store.generation()),
                Err(crate::artifacts::ArtifactError::Busy)
            ));
        }
        probe.release();
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.publications.active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let view = manager.artifact_revision("revision-fixture").await.unwrap();
        assert_eq!(view["current"]["artifact_set"], second);
        assert_eq!(view["previous"]["artifact_set"], first);
        let saved = Config::read(&manager.config_path).unwrap();
        let model = saved
            .models
            .iter()
            .find(|m| m.id == "revision-fixture")
            .unwrap();
        assert_eq!(model.artifact_set.as_ref(), Some(&second));
        assert_eq!(
            model.previous_artifact_set.as_ref().unwrap().artifact_set,
            first
        );
        assert!(!manager.publications.is_uncertain());
        assert!(manager.shutdown().await.is_settled());
    }

    #[tokio::test]
    async fn artifact_revision_warning_and_uncertain_commit_recover_the_saved_complete_binding() {
        for uncertain in [false, true] {
            let (_directory, manager, first, second, revision) = artifact_fixture().await;
            let path = manager.config_path.clone();
            let probe = Probe::new();
            probe.release();
            if uncertain {
                probe.panic_after_commit.store(true, Ordering::Release);
            } else {
                probe.warning_after_commit.store(true, Ordering::Release);
            }
            *manager.publication_probe.lock().unwrap() = Some(probe);
            let error = manager
                .set_artifact_revision(
                    "revision-fixture".into(),
                    revision,
                    crate::supervisor::ArtifactRevisionChange::Promote {
                        artifact_set: second.clone(),
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                if uncertain {
                    "configuration_publication_uncertain"
                } else {
                    "configuration_durability"
                }
            );
            assert_eq!(manager.publications.is_uncertain(), uncertain);
            let saved = Config::read(&path).unwrap();
            let spec = saved
                .models
                .iter()
                .find(|m| m.id == "revision-fixture")
                .unwrap();
            assert_eq!(spec.artifact_set.as_ref(), Some(&second));
            assert_eq!(
                spec.previous_artifact_set.as_ref().unwrap().artifact_set,
                first
            );
            let store = manager.artifacts().unwrap();
            for set in [&first, &second] {
                assert_eq!(
                    store.artifact_set(set).unwrap().references,
                    ["model:revision-fixture"]
                );
            }
            if uncertain {
                assert!(manager.pause(false).is_err());
            }
            assert_eq!(manager.shutdown().await.configuration_settled, !uncertain);
            drop(store);
            drop(manager);
            let recovered = crate::storage::after_test_release(path.parent().unwrap(), || {
                Manager::open(path.clone())
            });
            let view = recovered
                .artifact_revision("revision-fixture")
                .await
                .unwrap();
            assert_eq!(view["current"]["artifact_set"], second);
            assert_eq!(view["previous"]["artifact_set"], first);
            assert_eq!(view["worker_in_use"], false);
            assert!(!recovered.publications.is_uncertain());
            assert!(recovered.shutdown().await.is_settled());
        }
    }

    #[tokio::test]
    async fn aborted_policy_caller_retains_serialization_until_disk_and_memory_publication() {
        let (_directory, manager) = fixture();
        let probe = Probe::new();
        let _release = ReleaseProbe(probe.clone());
        *manager.publication_probe.lock().unwrap() = Some(probe.clone());
        let caller_manager = manager.clone();
        let caller =
            tokio::spawn(
                async move { caller_manager.set_policy("one", Policy::OnDemand, 0).await },
            );
        probe.wait_entered().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let worker = manager.worker("one").unwrap();
        assert!(
            manager.config.try_lock().is_err(),
            "aborting the caller released configuration serialization while disk publication was pending"
        );
        assert!(
            worker.state.try_lock().is_err(),
            "aborting the caller released worker serialization while disk publication was pending"
        );
        probe.release();
        let completed = tokio::time::timeout(Duration::from_secs(2), manager.config.lock())
            .await
            .unwrap();
        assert_eq!(completed.generation, 1);
        assert_eq!(completed.models[0].policy, Policy::OnDemand);
        assert_eq!(completed.models[0].revision, 1);
        let disk = Config::read(&manager.config_path).unwrap();
        assert_eq!(disk.generation, completed.generation);
        assert_eq!(disk.models[0].policy, completed.models[0].policy);
        assert_eq!(disk.models[0].revision, completed.models[0].revision);
        drop(completed);
        let state = worker.state.lock().await;
        assert_eq!(state.policy, Policy::OnDemand);
        assert_eq!(state.revision, 1);
        assert!(state.running.is_none());
        drop(state);
        assert_eq!(
            worker
                .observation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .revision,
            1
        );
        *manager.publication_probe.lock().unwrap() = None;
        let updated = manager
            .set_policy("one", Policy::Disabled, 1)
            .await
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.policy, Policy::Disabled);
        assert_eq!(
            Config::read(&manager.config_path).unwrap().models[0].revision,
            2
        );
        assert!(fs::read_dir(manager.config_root.as_ref().unwrap().root()).is_ok());
    }

    #[tokio::test]
    async fn rejected_disk_write_preserves_memory_and_allows_retry() {
        let (_directory, manager) = fixture();
        let original = fs::read(&manager.config_path).unwrap();
        let backup = manager.config_path.with_extension("backup");
        fs::rename(&manager.config_path, &backup).unwrap();
        fs::create_dir(&manager.config_path).unwrap();
        fs::write(manager.config_path.join("foreign"), b"unchanged").unwrap();
        let error = manager
            .set_policy("one", Policy::OnDemand, 0)
            .await
            .unwrap_err();
        assert_eq!(error.code, "policy_save");
        assert_eq!(manager.config.lock().await.generation, 0);
        let worker = manager.worker("one").unwrap();
        assert_eq!(worker.state.lock().await.policy, Policy::Disabled);
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(
            fs::read(manager.config_path.join("foreign")).unwrap(),
            b"unchanged"
        );
        assert_eq!(manager.publications.active(), 0);
        assert!(!manager.publications.is_uncertain());
        fs::remove_file(manager.config_path.join("foreign")).unwrap();
        fs::remove_dir(&manager.config_path).unwrap();
        fs::rename(backup, &manager.config_path).unwrap();
        assert_eq!(
            manager
                .set_policy("one", Policy::OnDemand, 0)
                .await
                .unwrap()
                .revision,
            1
        );
        assert!(
            manager
                .publications
                .drain(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
        );
    }

    #[tokio::test]
    async fn known_committed_warning_publishes_memory_before_returning_warning() {
        let (_directory, manager) = fixture();
        let probe = Probe::new();
        probe.release();
        probe.warning_after_commit.store(true, Ordering::Release);
        *manager.publication_probe.lock().unwrap() = Some(probe);
        let error = manager
            .set_policy("one", Policy::OnDemand, 0)
            .await
            .unwrap_err();
        assert_eq!(error.code, "policy_durability");
        let saved = Config::read(&manager.config_path).unwrap();
        assert_eq!(saved.generation, 1);
        assert_eq!(manager.config.lock().await.generation, saved.generation);
        let worker = manager.worker("one").unwrap();
        assert_eq!(worker.state.lock().await.policy, Policy::OnDemand);
        assert_eq!(
            worker
                .observation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .revision,
            1
        );
        assert!(
            manager
                .publications
                .drain(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
        );
    }

    #[tokio::test]
    async fn synthetic_postcommit_panic_latches_uncertainty_and_refuses_next_publication() {
        let (_directory, manager) = fixture();
        let probe = Probe::new();
        probe.release();
        probe.panic_after_commit.store(true, Ordering::Release);
        *manager.publication_probe.lock().unwrap() = Some(probe);
        assert!(
            manager
                .set_policy("one", Policy::OnDemand, 0)
                .await
                .is_err()
        );
        assert_eq!(Config::read(&manager.config_path).unwrap().generation, 1);
        assert_eq!(manager.config.lock().await.generation, 0);
        assert!(manager.publications.is_uncertain());
        assert!(manager.paused.load(Ordering::Acquire));
        assert_eq!(
            manager.pause(false).unwrap_err().code,
            "configuration_publication_uncertain"
        );
        assert!(manager.paused.load(Ordering::Acquire));
        assert_eq!(manager.publications.active(), 0);
        assert!(
            !manager
                .publications
                .drain(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
        );
        assert_eq!(
            manager
                .set_policy("one", Policy::OnDemand, 0)
                .await
                .unwrap_err()
                .code,
            "configuration_publication_uncertain"
        );
        assert_eq!(Config::read(&manager.config_path).unwrap().generation, 1);
    }

    #[tokio::test]
    async fn drain_timeout_keeps_publication_owned_until_real_settlement() {
        let (_directory, manager) = fixture();
        let probe = Probe::new();
        let _release = ReleaseProbe(probe.clone());
        *manager.publication_probe.lock().unwrap() = Some(probe.clone());
        let publishing = manager.clone();
        let caller =
            tokio::spawn(async move { publishing.set_policy("one", Policy::OnDemand, 0).await });
        probe.wait_entered().await;
        assert_eq!(manager.publications.active(), 1);
        assert!(
            !manager
                .publications
                .drain(tokio::time::Instant::now() + Duration::from_millis(5))
                .await
        );
        assert!(manager.config.try_lock().is_err());
        assert!(manager.worker("one").unwrap().state.try_lock().is_err());
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(manager.publications.active(), 1);
        probe.release();
        assert!(
            manager
                .publications
                .drain(tokio::time::Instant::now() + Duration::from_secs(2))
                .await
        );
        assert_eq!(manager.config.lock().await.generation, 1);
        assert_eq!(
            manager
                .set_policy("one", Policy::Disabled, 1)
                .await
                .unwrap_err()
                .code,
            "configuration_publication_closed"
        );
    }

    #[tokio::test]
    async fn config_only_outcome_captures_committed_generation() {
        let (_directory, manager) = fixture();
        let guard = manager.config.clone().lock_owned().await;
        let mut next = guard.clone();
        next.generation = 1;
        let outcome = manager
            .publish_configuration(guard, next, Effect::ConfigOnly)
            .await
            .unwrap();
        assert_eq!(outcome.generation, 1);
        assert!(outcome.model.is_none());
        assert!(!outcome.warning);
        assert_eq!(Config::read(&manager.config_path).unwrap().generation, 1);
        assert_eq!(manager.publications.active(), 0);
    }

    #[cfg(unix)]
    mod runtime_revisions {
        use super::*;
        use crate::supervisor::RuntimeChange;
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;
        struct Fixture(tempfile::TempDir);
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(entries) = fs::read_dir(self.0.path().join("runtime-packages")) {
                    for entry in entries.flatten() {
                        let payload = entry.path().join("payload");
                        if fs::symlink_metadata(&payload).is_ok_and(|m| m.is_dir()) {
                            let _ = fs::set_permissions(payload, fs::Permissions::from_mode(0o700));
                        }
                    }
                }
            }
        }
        fn fixture() -> (Fixture, Arc<Manager>, String, String) {
            let fixture = Fixture(tempfile::tempdir().unwrap());
            let root = fixture.0.path();
            let path = root.join("config.json");
            let config: Config = serde_json::from_value(serde_json::json!({"schema_version":1,"inhibit_suspend":false,"resident_budget_bytes":1024,"startup_timeout_ms":2000,"request_timeout_ms":1000,"models":[]})).unwrap();
            config.persist(&path).unwrap();
            let source = root.join("source");
            fs::create_dir(&source).unwrap();
            fs::write(source.join("engine"), b"fixture").unwrap();
            let input = root.join("input.json");
            let mut ids = Vec::new();
            for build in ["first", "second"] {
                let request = serde_json::json!({"schema_version":1,"bundle":{"schema_version":1,"implementation_id":"llama-cpp","root":source,"entrypoint":"engine","library_directories":[],"files":[{"path":"engine","sha256":format!("{:x}", Sha256::digest(b"fixture")),"role":"executable"}]},"files":[{"path":"engine","bytes":7}],"provenance":{"builder_id":"publication-fixture","source_revision":"synthetic","build_revision":build,"declared_licenses":["unknown"],"declared_sbom":null},"declared_target":{"operating_system":std::env::consts::OS,"architecture":std::env::consts::ARCH,"abi":"unknown","compute_paths":["cpu"]}});
                fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
                ids.push(
                    crate::storage::after_test_release(root, || {
                        crate::runtime_packages::import_file(
                            path.clone(),
                            input.clone(),
                            Arc::new(AtomicBool::new(false)),
                        )
                        .map_err(|error| {
                            std::io::Error::new(
                                if error.reason() == "owner_busy" {
                                    std::io::ErrorKind::WouldBlock
                                } else {
                                    std::io::ErrorKind::Other
                                },
                                error.to_string(),
                            )
                        })
                    })
                    .package,
                );
            }
            fs::write(&path, serde_json::to_vec(&serde_json::json!({"schema_version":2,"inhibit_suspend":false,"resident_budget_bytes":1024,"startup_timeout_ms":2000,"request_timeout_ms":1000,"models":[{"id":"runtime-fixture","model_file":"weights.gguf","execution_profile":{"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":ids[0],"args":["--model","{model}","--port","{port}"]},"resident_bytes":1,"max_inflight":1,"operations":["completions"],"policy":"disabled"}]})).unwrap()).unwrap();
            let manager = crate::storage::after_test_release(root, || Manager::open(path.clone()));
            let second = ids.pop().unwrap();
            let first = ids.pop().unwrap();
            (fixture, manager, first, second)
        }
        #[tokio::test]
        async fn cancelled_runtime_revision_caller_keeps_actual_publication_and_package_owners() {
            let (_fixture, manager, first, second) = fixture();
            let probe = Probe::new();
            let _release = ReleaseProbe(probe.clone());
            *manager.publication_probe.lock().unwrap() = Some(probe.clone());
            let caller_manager = manager.clone();
            let target = second.clone();
            let caller = tokio::spawn(async move {
                caller_manager
                    .set_runtime_selection(
                        "runtime-fixture".into(),
                        0,
                        RuntimeChange::Promote { package: target },
                    )
                    .await
            });
            probe.wait_entered().await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert!(manager.config.try_lock().is_err());
            assert!(
                manager
                    .worker("runtime-fixture")
                    .unwrap()
                    .state
                    .try_lock()
                    .is_err()
            );
            assert!(manager.publications.active() >= 2);
            let store = manager.runtime_packages.get().unwrap().clone();
            for id in [&first, &second] {
                store
                    .hold(
                        id,
                        crate::config::Provider::LlamaCpp,
                        Instant::now() + Duration::from_secs(1),
                    )
                    .unwrap()
                    .verify()
                    .unwrap();
            }
            probe.release();
            tokio::time::timeout(Duration::from_secs(2), async {
                while manager.publications.active() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let view = manager.runtime_selection("runtime-fixture").await.unwrap();
            assert_eq!(view["current"], second);
            assert_eq!(view["previous"], first);
            assert_eq!(view["policy"], "disabled");
            assert!(manager.shutdown().await.is_settled());
        }
        #[tokio::test]
        async fn runtime_warning_and_uncertain_commit_recover_saved_selection_and_complete_history()
        {
            for uncertain in [false, true] {
                let (_fixture, manager, first, second) = fixture();
                let path = manager.config_path.clone();
                let probe = Probe::new();
                probe.release();
                if uncertain {
                    probe.panic_after_commit.store(true, Ordering::Release);
                } else {
                    probe.warning_after_commit.store(true, Ordering::Release);
                }
                *manager.publication_probe.lock().unwrap() = Some(probe);
                let error = manager
                    .set_runtime_selection(
                        "runtime-fixture".into(),
                        0,
                        RuntimeChange::Promote {
                            package: second.clone(),
                        },
                    )
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code,
                    if uncertain {
                        "configuration_publication_uncertain"
                    } else {
                        "configuration_durability"
                    }
                );
                assert_eq!(manager.publications.is_uncertain(), uncertain);
                let saved = Config::read(&path).unwrap();
                assert_eq!(
                    saved.models[0].execution_profile.runtime_package(),
                    Some(second.as_str())
                );
                assert_eq!(
                    saved.models[0].execution_profile.previous_runtime_package(),
                    Some(first.as_str())
                );
                assert_eq!(manager.shutdown().await.configuration_settled, !uncertain);
                drop(manager);
                let recovered = crate::storage::after_test_release(path.parent().unwrap(), || {
                    Manager::open(path.clone())
                });
                let view = recovered
                    .runtime_selection("runtime-fixture")
                    .await
                    .unwrap();
                assert_eq!(view["current"], second);
                assert_eq!(view["previous"], first);
                assert_eq!(view["worker_in_use"], false);
                assert!(recovered.shutdown().await.is_settled());
            }
        }
    }
}
