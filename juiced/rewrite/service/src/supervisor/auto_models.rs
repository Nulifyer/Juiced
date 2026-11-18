use super::*;
use crate::library::LocalInventory;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

/// A file that appeared while the service runs may still be arriving (an upload's projector,
/// a tool writing into a shared cache), so it is registered once it has stayed unchanged.
const SETTLE: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_secs(300);
const KICK: Duration = Duration::from_secs(30);
const ADOPTION_WAIT: Duration = Duration::from_secs(2);
const NATIVE_SETTINGS: [&str; 2] = ["user_models.json", "recipe_options.json"];

/// A Hugging Face cache inside a Juiced 0.9 volume: `ROOT/huggingface/hub` beside
/// `ROOT/config` with the native model settings.
fn native_volume(hub: &std::path::Path) -> bool {
    hub.parent()
        .and_then(std::path::Path::parent)
        .is_some_and(|root| {
            NATIVE_SETTINGS
                .iter()
                .any(|name| root.join("config").join(name).is_file())
        })
}

/// Failures another pass can clear without the files changing.
const TRANSIENT: &[&str] = &[
    "engine_not_installed",
    "hardware_detection_unavailable",
    "hardware_detection_deadline",
    "no_usable_backend",
    "backend_not_selectable",
    "artifact_busy",
    "artifact_revision_conflict",
    "artifact_operation_interrupted",
    "artifact_inspection_interrupted",
    "revision_conflict",
    "model_exists",
    "alias_conflict",
    "storage_root_unavailable",
    "insufficient_storage",
    "model_exceeds_memory_budget",
];

#[derive(Clone)]
enum Outcome {
    Seen(Instant),
    /// The files cannot be a model as they are; they are tried again only when they change.
    Unusable {
        code: String,
        message: String,
    },
    Failed {
        code: String,
        message: String,
        at: Instant,
    },
}

#[derive(Clone)]
struct Verdict {
    bytes: u64,
    outcome: Outcome,
}

#[derive(Default)]
pub(super) struct AutoModels {
    running: AtomicBool,
    again: AtomicBool,
    booted: AtomicBool,
    retry: AtomicBool,
    last: Mutex<Option<Instant>>,
    /// When the next scheduled pass runs, so waits don't pile up one timer per pass.
    wake: Mutex<Option<Instant>>,
    verdicts: Mutex<BTreeMap<String, Verdict>>,
}

fn verdict(failure: AddModelError) -> Outcome {
    match failure {
        AddModelError::NotInstalled(body) => {
            let install = &body["install"];
            Outcome::Failed {
                code: "engine_not_installed".into(),
                message: format!(
                    "Needs the {} {} engine. Install it under Engines.",
                    install["engine"].as_str().unwrap_or("matching"),
                    install["backend"].as_str().unwrap_or_default()
                ),
                at: Instant::now(),
            }
        }
        AddModelError::Api(error) => {
            if TRANSIENT.contains(&error.code) || error.status.is_server_error() {
                Outcome::Failed {
                    code: error.code.to_owned(),
                    message: error.message.clone(),
                    at: Instant::now(),
                }
            } else {
                Outcome::Unusable {
                    code: error.code.to_owned(),
                    message: error.message.clone(),
                }
            }
        }
    }
}

impl Manager {
    /// Starts a background pass that registers, as Disabled, every complete model file on
    /// disk that no model uses. A pass already running repeats once it finishes.
    pub fn register_local_models(self: &Arc<Self>) {
        if self.native_import_only || self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let state = &self.auto_models;
        if state.running.swap(true, Ordering::AcqRel) {
            state.again.store(true, Ordering::Release);
            return;
        }
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                manager.auto_models.again.store(false, Ordering::Release);
                let wait = manager.register_pass().await;
                if manager.auto_models.again.load(Ordering::Acquire) {
                    continue;
                }
                manager.auto_models.running.store(false, Ordering::Release);
                if manager.auto_models.again.swap(false, Ordering::AcqRel) {
                    if manager.auto_models.running.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    continue;
                }
                if let Some(wait) = wait {
                    manager.register_after(wait);
                }
                return;
            }
        });
    }

    fn register_after(self: &Arc<Self>, wait: Duration) {
        let due = Instant::now() + wait;
        {
            let mut wake = self
                .auto_models
                .wake
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if wake.is_some_and(|scheduled| scheduled <= due) {
                return;
            }
            *wake = Some(due);
        }
        let manager = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let mut wake = manager
                .auto_models
                .wake
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if *wake == Some(due) {
                *wake = None;
            }
            drop(wake);
            manager.register_local_models();
        });
    }

    /// Runs the first pass and another whenever a download stops, for the service's lifetime.
    pub fn watch_local_models(self: &Arc<Self>) {
        self.register_local_models();
        let manager = Arc::downgrade(self);
        let library = self.library.clone();
        tokio::spawn(async move {
            loop {
                library.pull_finished().await;
                let Some(manager) = manager.upgrade() else {
                    return;
                };
                if manager.shutting_down.load(Ordering::Acquire) {
                    return;
                }
                manager.register_local_models();
            }
        });
    }

    /// Starts a pass when the last one is old, so listing models finds files that appeared
    /// on disk without every request paying for a scan.
    pub fn refresh_local_models(self: &Arc<Self>) {
        let due = self
            .auto_models
            .last
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_none_or(|last| last.elapsed() >= KICK);
        if due {
            self.register_local_models();
        }
    }

    /// Starts a pass that tries failed files again at once, for when an installed engine
    /// may have fixed them.
    pub(super) fn retry_local_models(self: &Arc<Self>) {
        for pull in self.library.engine_blocked_follow_ups(false) {
            if self.library.retry_follow_up(&pull).is_ok() {
                self.follow_pull(pull);
            }
        }
        self.auto_models.retry.store(true, Ordering::Release);
        self.register_local_models();
    }

    async fn register_pass(self: &Arc<Self>) -> Option<Duration> {
        let state = &self.auto_models;
        let first = !state.booted.swap(true, Ordering::AcqRel);
        let retry = state.retry.swap(false, Ordering::AcqRel);
        *state.last.lock().unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
        // Native adoption registers these files with their native names and settings.
        if self.native_adoption.busy() {
            state.booted.store(!first, Ordering::Release);
            state.retry.fetch_or(retry, Ordering::AcqRel);
            return Some(ADOPTION_WAIT);
        }
        let adopting: BTreeSet<PathBuf> = self
            .native_adoption
            .unregistered_weights()
            .into_iter()
            .flat_map(|path| [std::fs::canonicalize(&path).ok(), Some(path)])
            .flatten()
            .collect();
        let inventory = self.local_library(true).await.ok()?;
        self.settle_finished_pulls(&inventory);
        let claimed = self.library.claimed_artifacts();
        // A Juiced 0.9 volume mounted beside Juiced's own data: its settings import registers
        // these files, so registering them here first would make the import skip them.
        let foreign: Vec<&std::path::Path> = inventory
            .hubs
            .iter()
            .filter(|hub| !hub.owned && native_volume(&hub.path))
            .map(|hub| hub.path.as_path())
            .collect();
        let mut live = BTreeSet::new();
        let mut wait: Option<Duration> = None;
        let mut registered = 0usize;
        for group in &inventory.groups {
            for quant in &group.quants {
                if self.shutting_down.load(Ordering::Acquire) {
                    return None;
                }
                let Some(add) = quant.add.as_ref() else {
                    continue;
                };
                if !quant.used_by.is_empty() || !quant.complete {
                    continue;
                }
                let members: Vec<_> = group
                    .files
                    .iter()
                    .filter(|file| quant.files.contains(&file.id))
                    .collect();
                let downloading = members
                    .iter()
                    .filter_map(|file| file.artifact.as_ref())
                    .any(|artifact| claimed.contains(artifact));
                let adopted = members.iter().any(|file| {
                    adopting.contains(&file.canonical) || adopting.contains(&file.location)
                });
                if downloading || adopted {
                    continue;
                }
                live.insert(quant.file.clone());
                if members
                    .iter()
                    .any(|file| foreign.iter().any(|hub| file.location.starts_with(hub)))
                {
                    self.record(
                        &quant.file,
                        quant.bytes,
                        Outcome::Unusable {
                            code: "native_volume".into(),
                            message: "Part of a Juiced 0.9 volume. Import that volume's settings to use it, or link the file with Add file.".into(),
                        },
                    );
                    continue;
                }

                let known = state
                    .verdicts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&quant.file)
                    .filter(|verdict| verdict.bytes == quant.bytes)
                    .map(|verdict| verdict.outcome.clone());
                let later = |left: Duration| Some(wait.map_or(left, |known| known.min(left)));
                let failed_before = match &known {
                    Some(Outcome::Failed { code, .. }) => Some(code.clone()),
                    _ => None,
                };
                match known {
                    Some(Outcome::Unusable { .. }) => continue,
                    Some(Outcome::Failed { at, .. }) if !retry && at.elapsed() < RETRY => {
                        wait = later(RETRY - at.elapsed());
                        continue;
                    }
                    Some(Outcome::Seen(at)) if at.elapsed() < SETTLE => {
                        wait = later(SETTLE - at.elapsed());
                        continue;
                    }
                    None if !first => {
                        self.record(&quant.file, quant.bytes, Outcome::Seen(Instant::now()));
                        wait = later(SETTLE);
                        continue;
                    }
                    _ => {}
                }
                let Ok(mut request) = serde_json::from_value::<ModelAdd>(add.clone()) else {
                    continue;
                };
                request.automatic = true;
                // A cache snapshot links to a blob named by its digest, so the model is named
                // after the snapshot file.
                if request.path.is_some() {
                    request.id = group
                        .files
                        .iter()
                        .find(|file| file.id == quant.file)
                        .and_then(|file| crate::engines::models::default_id(&file.name));
                }
                match crate::native_api::register_download(self, request).await {
                    Ok(reply) => {
                        registered += 1;
                        state
                            .verdicts
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&quant.file);
                        self.logs.service(
                            crate::logs::Level::Info,
                            &format!(
                                "Registered {} from {} {} as Disabled",
                                reply["model"]["id"].as_str().unwrap_or("a model"),
                                group.name,
                                quant.label
                            ),
                        );
                    }
                    Err(failure) => {
                        let outcome = verdict(failure);
                        // A file retried every few minutes is logged once per reason.
                        if let Outcome::Failed { code, message, .. }
                        | Outcome::Unusable { code, message } = &outcome
                            && failed_before.as_ref() != Some(code)
                        {
                            self.logs.service(
                                crate::logs::Level::Warning,
                                &format!(
                                    "{} {} on disk is not a model yet ({code}): {message}",
                                    group.name, quant.label
                                ),
                            );
                        }
                        if matches!(outcome, Outcome::Failed { .. }) {
                            wait = later(RETRY);
                        }
                        self.record(&quant.file, quant.bytes, outcome);
                    }
                }
            }
        }
        state
            .verdicts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|key, _| live.contains(key));
        if registered > 0 {
            self.library.invalidate_local();
        }
        wait
    }

    fn record(&self, key: &str, bytes: u64, outcome: Outcome) {
        self.auto_models
            .verdicts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key.to_owned(), Verdict { bytes, outcome });
    }

    /// A finished download leaves the list once a model runs its files, or at once when it
    /// asked for no model; its files then register like any others found on disk.
    fn settle_finished_pulls(&self, inventory: &LocalInventory) {
        let used: BTreeSet<&str> = inventory
            .groups
            .iter()
            .flat_map(|group| &group.files)
            .filter(|file| !file.used_by.is_empty())
            .filter_map(|file| file.artifact.as_deref())
            .collect();
        let native = crate::native_api::active_pulls();
        self.library.release_finished(
            |artifact| used.contains(artifact),
            |pull| native.contains(pull),
        );
    }

    /// The local inventory with, for each complete quant no model uses, why it is not a
    /// model: `pending` until a pass reaches it, or the failure.
    pub async fn local_view(self: &Arc<Self>, fresh: bool) -> Result<Value, ApiError> {
        let inventory = self.local_library(fresh).await?;
        if fresh {
            self.register_local_models();
        }
        let mut view = serde_json::to_value(&*inventory).map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "local_inventory_interrupted",
                "Listing the files on this machine was interrupted. Retry.",
            )
        })?;
        let verdicts = self
            .auto_models
            .verdicts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let downloads = self.library.failed_follow_ups();
        for quant in view["groups"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .flat_map(|group| group["quants"].as_array_mut().into_iter().flatten())
        {
            let unused = quant["used_by"].as_array().is_some_and(Vec::is_empty);
            if !unused || quant["add"].is_null() || quant["complete"] != true {
                continue;
            }
            let known = quant["file"].as_str().and_then(|file| verdicts.get(file));
            let download = quant["files"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|file| downloads.get(file.as_str()?.strip_prefix("artifact:")?));
            quant["install"] = match (known.map(|verdict| &verdict.outcome), download) {
                (_, Some((code, message))) => {
                    json!({"state": "failed", "error": code, "message": message})
                }
                (Some(Outcome::Unusable { code, message }), _) => {
                    json!({"state": "unusable", "error": code, "message": message})
                }
                (Some(Outcome::Failed { code, message, .. }), _) => {
                    json!({"state": "failed", "error": code, "message": message})
                }
                _ => json!({"state": "pending", "error": null, "message": null}),
            };
        }
        Ok(view)
    }

    /// Why a file no model uses is not a model, for the storage view.
    pub(super) fn local_notes(&self, inventory: &LocalInventory) -> BTreeMap<String, String> {
        let verdicts = self
            .auto_models
            .verdicts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut notes = BTreeMap::new();
        for quant in inventory.groups.iter().flat_map(|group| &group.quants) {
            let message = match verdicts.get(&quant.file).map(|verdict| &verdict.outcome) {
                Some(Outcome::Unusable { message, .. } | Outcome::Failed { message, .. }) => {
                    message
                }
                _ => continue,
            };
            for file in &quant.files {
                notes.insert(file.clone(), message.clone());
            }
        }
        notes
    }
}
