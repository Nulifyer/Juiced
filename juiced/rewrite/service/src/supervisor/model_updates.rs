use super::{AddModelError, EngineInstall, Manager, ModelAdd, publication};
use crate::{
    ApiError,
    config::Policy,
    library::{FollowUp, FollowUpOutcome, PullRequest},
};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

const MAXIMUM_REPOSITORIES: usize = 64;
const MAXIMUM_SOURCES: usize = 4096;

/// Where a registered model's main weights came from on the Hugging Face hub, with the local
/// file digest when it is known without reading the file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Origin {
    repo: String,
    file: String,
    revision: Option<String>,
    sha256: Option<String>,
}

fn digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Reads `…/models--ORG--NAME/snapshots/REVISION/FILE` from a Hugging Face cache path. Cache
/// snapshots link LFS files to `blobs/SHA256`, so the link names the digest.
fn cache_origin(path: &Path) -> Option<Origin> {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    let at = parts.iter().position(|part| part.starts_with("models--"))?;
    let (organization, name) = parts[at].strip_prefix("models--")?.split_once("--")?;
    if parts.get(at + 1) != Some(&"snapshots") || parts.len() < at + 4 {
        return None;
    }
    let sha256 = std::fs::read_link(path)
        .ok()
        .and_then(|target| target.file_name()?.to_str().map(str::to_owned))
        .filter(|name| digest(name));
    Some(Origin {
        repo: format!("{organization}/{name}"),
        file: parts[at + 3..].join("/"),
        revision: Some(parts[at + 2].to_owned()),
        sha256,
    })
}

enum Outcome {
    Current,
    Newer,
    Failed(&'static str),
}

fn compare(origin: &Origin, revision: Option<&str>, files: &[(String, Option<String>)]) -> Outcome {
    let Some((_, remote)) = files.iter().find(|(path, _)| *path == origin.file) else {
        return Outcome::Failed("file_not_upstream");
    };
    match (&origin.sha256, remote) {
        (Some(local), Some(remote)) => {
            if local.eq_ignore_ascii_case(remote) {
                Outcome::Current
            } else {
                Outcome::Newer
            }
        }
        _ => match (&origin.revision, revision) {
            (Some(local), Some(remote)) if local == remote => Outcome::Current,
            (Some(_), Some(_)) => Outcome::Newer,
            _ => Outcome::Failed("digest_unknown"),
        },
    }
}

fn managed_origin(
    library: &crate::library::Library,
    sources: &[crate::artifacts::SourceRecord],
    artifact: &str,
) -> Option<Origin> {
    let record = sources.iter().find(|record| record.sha256 == artifact)?;
    let (repo, revision, file) = library.hub().parse_resolve_url(&record.source_url)?;
    Some(Origin {
        repo,
        file,
        revision: Some(revision),
        sha256: Some(record.sha256.clone()),
    })
}

impl Manager {
    async fn hub_sources(&self) -> Vec<crate::artifacts::SourceRecord> {
        match (self.artifacts(), self.library().hub().base_prefix()) {
            (Ok(store), Some(prefix)) => {
                let prefix = format!("{prefix}/");
                tokio::task::spawn_blocking(move || {
                    store
                        .completed_sources(&prefix, MAXIMUM_SOURCES)
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// Compares each downloaded model's main weights with the current upstream file, as native
    /// `POST /models/check-updates` did. Models from other sources are not checked.
    pub async fn check_model_updates(self: &Arc<Self>) -> Result<Value, ApiError> {
        let library = self.library().clone();
        if let Some(error) = library.hub().blocked() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "update_check_offline",
                format!(
                    "Cannot check model updates while the hub is unavailable ({}).",
                    error.code()
                ),
            ));
        }
        let parent = self
            .config_path
            .parent()
            .expect("configuration parent")
            .to_owned();
        let models: Vec<(String, Option<String>, std::path::PathBuf)> = {
            let config = self.config.lock().await;
            config
                .models
                .iter()
                .map(|model| {
                    let file = if model.model_file.is_relative() {
                        parent.join(&model.model_file)
                    } else {
                        model.model_file.clone()
                    };
                    (model.id.clone(), model.artifact.clone(), file)
                })
                .collect()
        };
        let sources = self.hub_sources().await;
        let hub = library.clone();
        let origins: Vec<(String, Option<Origin>)> = tokio::task::spawn_blocking(move || {
            models
                .into_iter()
                .map(|(id, artifact, file)| {
                    let managed = artifact
                        .as_deref()
                        .and_then(|artifact| managed_origin(&hub, &sources, artifact));
                    (id, managed.or_else(|| cache_origin(&file)))
                })
                .collect()
        })
        .await
        .unwrap_or_default();
        let mut repositories: Vec<String> = origins
            .iter()
            .filter_map(|(_, origin)| origin.as_ref().map(|origin| origin.repo.clone()))
            .collect();
        repositories.sort();
        repositories.dedup();
        let mut upstream = BTreeMap::new();
        for repo in repositories.iter().take(MAXIMUM_REPOSITORIES) {
            let result = library
                .hub()
                .info(repo, None, true)
                .await
                .map(|(info, _)| {
                    (
                        info.revision.clone(),
                        info.files
                            .iter()
                            .map(|file| (file.path.clone(), file.sha256.clone()))
                            .collect::<Vec<_>>(),
                    )
                })
                .map_err(|error| error.code());
            upstream.insert(repo.clone(), result);
        }
        let mut updated = Vec::new();
        let mut failed = serde_json::Map::new();
        let mut determined = BTreeMap::new();
        let mut checked = 0usize;
        for (id, origin) in origins {
            let Some(origin) = origin else {
                continue;
            };
            checked += 1;
            let outcome = match upstream.get(&origin.repo) {
                None => Outcome::Failed("repository_limit"),
                Some(Err(code)) => Outcome::Failed(code),
                Some(Ok((revision, files))) => compare(&origin, revision.as_deref(), files),
            };
            match outcome {
                Outcome::Current => {
                    determined.insert(id, false);
                }
                Outcome::Newer => {
                    determined.insert(id.clone(), true);
                    updated.push(id);
                }
                Outcome::Failed(code) => {
                    failed.insert(id, json!(code));
                }
            }
        }
        library.record_updates(determined);
        Ok(json!({
            "status": if failed.is_empty() { "success" } else { "failed" },
            "updates_available": updated.len(),
            "models": updated,
            "failed_models": failed,
            "checked": checked,
        }))
    }
}

const FOLLOW_POLL: Duration = Duration::from_millis(500);
/// Registration failures that clear on their own, and the waits before each retry.
const ADD_TRANSIENT: &[&str] = &[
    "artifact_busy",
    "artifact_revision_conflict",
    "artifact_operation_interrupted",
    "artifact_inspection_interrupted",
    "hardware_detection_unavailable",
    "hardware_detection_deadline",
];
const ADD_RETRIES: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(15),
    Duration::from_secs(60),
];
const BUSY_RETRY: Duration = Duration::from_secs(2);
const BUSY_ATTEMPTS: u32 = 300;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUpdate {
    pub expected_revision: u64,
}

fn conflict(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

fn interrupted() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "artifact_operation_interrupted",
        "The file operation was interrupted. Retry.",
    )
}

fn failed(error: ApiError) -> FollowUpOutcome {
    FollowUpOutcome::failed(error.code, error.message)
}

impl Manager {
    /// Downloads the current upstream revision of a stored model's main file and, once it is
    /// complete, moves the model to it with its settings. The pull shows in Downloads.
    pub async fn update_model(
        self: &Arc<Self>,
        id: &str,
        request: ModelUpdate,
    ) -> Result<Value, ApiError> {
        let worker = self.worker(id)?;
        let artifact = {
            let state = worker.state.lock().await;
            self.ensure_registered(&worker)?;
            if state.revision != request.expected_revision {
                return Err(conflict(
                    "revision_conflict",
                    "The model changed. Refresh and retry.",
                ));
            }
            worker
                .spec
                .artifact
                .clone()
                .filter(|_| {
                    worker.spec.execution_profile.native_model_id().is_none()
                        && worker.spec.resource_profile.is_none()
                })
                .ok_or_else(|| {
                    conflict(
                        "update_unsupported",
                        "Only models Juiced downloaded can update here. Download the new files from Discover.",
                    )
                })?
        };
        let library = self.library().clone();
        if let Some(error) = library.hub().blocked() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "update_check_offline",
                format!(
                    "Cannot update models while the hub is unavailable ({}).",
                    error.code()
                ),
            ));
        }
        let sources = self.hub_sources().await;
        let origin = managed_origin(&library, &sources, &artifact).ok_or_else(|| {
            conflict(
                "update_unsupported",
                "Juiced doesn't know where this model's files came from. Download the new files from Discover.",
            )
        })?;
        let store = self.artifacts()?;
        let then = FollowUp::Update {
            model: id.to_owned(),
            from_artifact: artifact,
        };
        let mut attempt = 0;
        let view = loop {
            let pull = PullRequest {
                repo: origin.repo.clone(),
                file: Some(origin.file.clone()),
                files: None,
                revision: None,
                expected_generation: store.generation(),
                then: None,
            };
            match library
                .pull_with(store.clone(), pull, Some(then.clone()))
                .await
            {
                Err(error) if error.status == StatusCode::CONFLICT && attempt < 3 => {
                    attempt += 1;
                    tokio::time::sleep(FOLLOW_POLL).await;
                }
                other => break other?,
            }
        };
        if let Some(pull) = view["pull"].as_str() {
            self.follow_pull(pull.to_owned());
        }
        Ok(view)
    }

    /// Resumes follow-ups of journaled pulls; call after the library recovered them.
    pub(crate) fn recover_follow_ups(self: &Arc<Self>) {
        // A shutdown during an engine install fails the follow-up; the restart picks it up.
        for pull in self.library().engine_blocked_follow_ups(true) {
            let _ = self.library().retry_follow_up(&pull);
        }
        for pull in self.library().pending_follow_ups() {
            self.follow_pull(pull);
        }
    }

    pub(crate) fn follow_pull(self: &Arc<Self>, pull: String) {
        if !self.library().claim_follow_up(&pull) {
            return;
        }
        let manager = self.clone();
        tokio::spawn(async move {
            match manager.run_follow_up(&pull).await {
                Some(outcome) => manager.library().settle_follow_up(&pull, outcome),
                None => manager.library().release_follow_up(&pull),
            }
            manager.register_local_models();
        });
    }

    async fn run_follow_up(self: &Arc<Self>, pull: &str) -> Option<FollowUpOutcome> {
        let then = self.library().follow_up(pull)?;
        let view = loop {
            if self.shutting_down.load(Ordering::Acquire) {
                return None;
            }
            let view = self.library().pull_view(pull).ok()?;
            match view["state"].as_str() {
                Some("complete") => break view,
                Some("failed") => {
                    return Some(FollowUpOutcome::failed(
                        "download_failed",
                        "The download didn't finish.",
                    ));
                }
                _ => tokio::time::sleep(FOLLOW_POLL).await,
            }
        };
        let set = view["set"].as_str().map(str::to_owned);
        let first = view["files"][0]["sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        Some(match then {
            FollowUp::Add { id, policy, engine } => {
                self.add_downloaded(first, set, id, policy, engine).await
            }
            FollowUp::Update {
                model,
                from_artifact,
            } => {
                let outcome = self
                    .switch_downloaded(&model, &from_artifact, first, set)
                    .await;
                if outcome.state == crate::library::FollowUpState::Done {
                    self.library()
                        .record_updates(BTreeMap::from([(model, false)]));
                    self.delete_superseded(&from_artifact).await;
                }
                outcome
            }
        })
    }

    async fn add_downloaded(
        self: &Arc<Self>,
        artifact: String,
        set: Option<String>,
        id: Option<String>,
        policy: Policy,
        engine: Option<String>,
    ) -> FollowUpOutcome {
        let request = || ModelAdd {
            artifact: set.is_none().then(|| artifact.clone()),
            artifact_set: set.clone(),
            id: id.clone(),
            policy: Some(policy),
            engine: engine.clone(),
            ..ModelAdd::default()
        };
        let mut installed = false;
        let mut waits = ADD_RETRIES.iter();
        loop {
            match crate::native_api::register_download(self, request()).await {
                Err(AddModelError::Api(error)) if ADD_TRANSIENT.contains(&error.code) => {
                    let Some(wait) = waits.next() else {
                        return failed(error);
                    };
                    if self.shutting_down.load(Ordering::Acquire) {
                        return failed(error);
                    }
                    tokio::time::sleep(*wait).await;
                }
                Ok(reply) => {
                    return FollowUpOutcome::done(
                        reply["model"]["id"].as_str().unwrap_or_default(),
                    );
                }
                Err(AddModelError::NotInstalled(body)) if !installed => {
                    installed = true;
                    if let Err(outcome) = self.install_for(&body).await {
                        return outcome;
                    }
                }
                Err(AddModelError::NotInstalled(_)) => {
                    return FollowUpOutcome::failed(
                        "engine_not_installed",
                        "The engine for this model isn't installed.",
                    );
                }
                Err(AddModelError::Api(error)) => return failed(error),
            }
        }
    }

    /// Installs the engine build `models/add` picked for this machine and waits for it.
    async fn install_for(self: &Arc<Self>, body: &Value) -> Result<(), FollowUpOutcome> {
        let install = &body["install"];
        let request = EngineInstall {
            engine: install["engine"].as_str().unwrap_or_default().to_owned(),
            backend: install["backend"].as_str().unwrap_or_default().to_owned(),
            expected_generation: install["expected_generation"].as_u64().unwrap_or_default(),
        };
        let (_, reply) = self.install_engine(request).await.map_err(failed)?;
        let Some(job) = reply["job"]["id"].as_str() else {
            return Ok(());
        };
        loop {
            if self.shutting_down.load(Ordering::Acquire) {
                return Err(FollowUpOutcome::failed(
                    "engine_install_interrupted",
                    "The engine install stopped. Retry.",
                ));
            }
            let view = self.engine_job(job).ok_or_else(|| {
                FollowUpOutcome::failed(
                    "engine_install_interrupted",
                    "The engine install stopped. Retry.",
                )
            })?;
            match view["state"].as_str() {
                Some("complete") => return Ok(()),
                Some("failed") => {
                    return Err(FollowUpOutcome::failed(
                        "engine_install_failed",
                        "The engine didn't install. Retry.",
                    ));
                }
                _ => tokio::time::sleep(FOLLOW_POLL).await,
            }
        }
    }

    /// Deletes the files an update replaced, unless a model still uses them; otherwise they
    /// would come back as a second model.
    async fn delete_superseded(self: &Arc<Self>, artifact: &str) {
        let Ok(inventory) = self.local_library(true).await else {
            return;
        };
        let id = format!("artifact:{artifact}");
        let files: Vec<String> = inventory
            .groups
            .iter()
            .flat_map(|group| &group.quants)
            .find(|quant| quant.files.contains(&id))
            .filter(|quant| quant.used_by.is_empty())
            .map(|quant| quant.files.clone())
            .unwrap_or_default();
        if !files.is_empty() {
            let _ = self.delete_unused(&files).await;
        }
    }

    async fn switch_downloaded(
        self: &Arc<Self>,
        model: &str,
        from: &str,
        first: String,
        set: Option<String>,
    ) -> FollowUpOutcome {
        let artifact = match &set {
            None => first,
            Some(set) => {
                let Ok(store) = self.artifacts() else {
                    return failed(interrupted());
                };
                let id = set.clone();
                let primary = tokio::task::spawn_blocking(move || {
                    let view = store.artifact_set(&id).ok()?;
                    view.specification
                        .members
                        .iter()
                        .find(|member| member.name == view.specification.primary)
                        .map(|member| member.artifact.clone())
                })
                .await
                .ok()
                .flatten();
                match primary {
                    Some(primary) => primary,
                    None => return failed(interrupted()),
                }
            }
        };
        if artifact == from {
            return FollowUpOutcome::done(model);
        }
        for _ in 0..BUSY_ATTEMPTS {
            match self
                .switch_model_files(model, from, artifact.clone(), set.clone())
                .await
            {
                Ok(()) => return FollowUpOutcome::done(model),
                Err(error) if matches!(error.code, "worker_busy" | "preparation_pending") => {
                    if self.shutting_down.load(Ordering::Acquire) {
                        return failed(error);
                    }
                    tokio::time::sleep(BUSY_RETRY).await;
                }
                Err(error) => return failed(error),
            }
        }
        FollowUpOutcome::failed(
            "worker_busy",
            "The model stayed busy, so it keeps its old files. Update again later.",
        )
    }

    /// Points a stored model at other files and keeps its saved settings. A loaded worker
    /// stops first and starts again on the new files; on failure the model keeps the old ones.
    async fn switch_model_files(
        self: &Arc<Self>,
        id: &str,
        from: &str,
        artifact: String,
        set: Option<String>,
    ) -> Result<(), ApiError> {
        let worker = self.worker(id)?;
        let mut state = worker.state.clone().lock_owned().await;
        self.ensure_registered(&worker)?;
        if worker.spec.artifact.as_deref() != Some(from) {
            return Err(conflict(
                "model_files_changed",
                "The model's files changed while the update downloaded. It keeps its current files.",
            ));
        }
        if worker.spec.artifact_set.is_some() && set.is_none() {
            return Err(conflict(
                "update_unsupported",
                "This model's files were combined when it was added. Add the new files as a new model.",
            ));
        }
        Self::reap(&mut state);
        if worker.active.load(Ordering::Acquire) != 0 || worker.preparing.load(Ordering::Acquire) {
            return Err(conflict(
                "worker_busy",
                "The model is busy. Retry when its requests finish.",
            ));
        }
        Self::process_available(&worker, state.running.is_some())?;
        let store = self.artifacts()?;
        let reference = format!("model:{id}");
        let companions = worker.spec.execution_profile.adapter.holds_companion_sets();
        let held = {
            let store = store.clone();
            let (artifact, set, reference) = (artifact.clone(), set.clone(), reference.clone());
            tokio::task::spawn_blocking(move || {
                let held = store.hold_binding_with(&artifact, set.as_deref(), companions)?;
                store.reference_binding(
                    &artifact,
                    set.as_deref(),
                    &reference,
                    true,
                    store.generation(),
                )?;
                Ok::<_, crate::artifacts::ArtifactError>(held)
            })
            .await
            .map_err(|_| interrupted())?
            .map_err(ApiError::from)?
        };
        let unreference_new = || {
            let store = store.clone();
            let (artifact, set, reference) = (artifact.clone(), set.clone(), reference.clone());
            tokio::task::spawn_blocking(move || {
                let _ = store.reference_binding(
                    &artifact,
                    set.as_deref(),
                    &reference,
                    false,
                    store.generation(),
                );
            })
        };
        let policy = state.policy;
        let config = self.config.clone().lock_owned().await;
        let mut next = config.clone();
        let prepared = (|| {
            next.generation = next.generation.checked_add(1).ok_or_else(|| {
                conflict(
                    "revision_exhausted",
                    "The configuration generation cannot advance.",
                )
            })?;
            let spec = next
                .models
                .iter_mut()
                .find(|spec| spec.id == id)
                .expect("registered model");
            spec.artifact = Some(artifact.clone());
            spec.artifact_set = set.clone();
            spec.previous_artifact_set = None;
            spec.model_file = held.path().to_owned();
            spec.policy = policy;
            spec.revision = next.generation;
            next.validate().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_registration",
                    "The new files cannot form a valid saved model registration.",
                )
            })?;
            Ok::<_, ApiError>(
                next.models
                    .iter()
                    .find(|spec| spec.id == id)
                    .expect("registered model")
                    .resolved(self.config_path.parent().expect("configuration parent")),
            )
        })();
        let replacement = match prepared {
            Ok(resolved) => match Self::check_config_size(next).await {
                Ok(checked) => {
                    next = checked;
                    match self.bind_runtime(resolved).await.and_then(|resolved| {
                        Self::create_worker(
                            resolved,
                            Some(Arc::new(held)),
                            &self.admission,
                            &self.processes,
                            Some(worker.slots.clone()),
                        )
                    }) {
                        Ok(replacement) => replacement,
                        Err(error) => {
                            let _ = unreference_new().await;
                            return Err(error);
                        }
                    }
                }
                Err(error) => {
                    let _ = unreference_new().await;
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = unreference_new().await;
                return Err(error);
            }
        };
        let stopped = state.running.is_some();
        if let Some(run) = state.running.as_mut() {
            if let Err(error) = run.child.stop().await {
                state.error = Some("worker_cleanup_pending");
                Self::publish(&worker, &state);
                let _ = unreference_new().await;
                return Err(super::process_error(error));
            }
            state.running = None;
            state.error = None;
            Self::publish(&worker, &state);
        }
        let previous = worker.clone();
        let outcome = match self
            .publish_configuration(
                config,
                next,
                publication::Effect::Replace {
                    previous: worker,
                    state,
                    replacement: replacement.clone(),
                },
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = unreference_new().await;
                if stopped {
                    let _ = self.restart_worker(&previous).await;
                }
                return Err(error);
            }
        };
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "The model moved to its new files but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        let configuration = self.config.clone().lock_owned().await;
        if configuration.generation == outcome.generation {
            let old = (
                previous.spec.artifact.clone(),
                previous.spec.artifact_set.clone(),
                previous.spec.previous_artifact_set.clone(),
            );
            let _ = tokio::task::spawn_blocking(move || {
                let _configuration = configuration;
                if let (Some(artifact), set, _) = &old {
                    let _ = store.reference_binding(
                        artifact,
                        set.as_deref(),
                        &reference,
                        false,
                        store.generation(),
                    );
                }
                if let (_, _, Some(rollback)) = &old {
                    let _ = store.reference_binding(
                        &rollback.artifact,
                        Some(&rollback.artifact_set),
                        &reference,
                        false,
                        store.generation(),
                    );
                }
            })
            .await;
        } else {
            drop(configuration);
        }
        if stopped || policy == Policy::Hot {
            let _ = self.restart_worker(&replacement).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_name_repository_revision_file_and_linked_digest() {
        let directory = tempfile::tempdir().unwrap();
        let blob = "a".repeat(64);
        let root = directory.path().join("hub/models--acme--Tiny-GGUF");
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::create_dir_all(root.join("snapshots/0123abc/sub")).unwrap();
        std::fs::write(root.join("blobs").join(&blob), b"weights").unwrap();
        let file = root.join("snapshots/0123abc/sub/tiny-Q4_0.gguf");
        #[cfg(unix)]
        std::os::unix::fs::symlink(format!("../../../blobs/{blob}"), &file).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&file, b"weights").unwrap();
        let origin = cache_origin(&file).unwrap();
        assert_eq!(origin.repo, "acme/Tiny-GGUF");
        assert_eq!(origin.file, "sub/tiny-Q4_0.gguf");
        assert_eq!(origin.revision.as_deref(), Some("0123abc"));
        #[cfg(unix)]
        assert_eq!(origin.sha256.as_deref(), Some(blob.as_str()));
        assert!(cache_origin(Path::new("/models/plain.gguf")).is_none());
    }

    #[test]
    fn digests_decide_before_revisions() {
        let origin = Origin {
            repo: "acme/x".into(),
            file: "x.gguf".into(),
            revision: Some("old".into()),
            sha256: Some("b".repeat(64)),
        };
        let same = vec![("x.gguf".to_owned(), Some("b".repeat(64)))];
        let changed = vec![("x.gguf".to_owned(), Some("c".repeat(64)))];
        assert!(matches!(
            compare(&origin, Some("new"), &same),
            Outcome::Current
        ));
        assert!(matches!(
            compare(&origin, Some("old"), &changed),
            Outcome::Newer
        ));
        assert!(matches!(
            compare(&origin, Some("new"), &[]),
            Outcome::Failed("file_not_upstream")
        ));
        let copied = Origin {
            sha256: None,
            ..origin
        };
        assert!(matches!(
            compare(&copied, Some("new"), &same),
            Outcome::Newer
        ));
        assert!(matches!(
            compare(&copied, Some("old"), &same),
            Outcome::Current
        ));
    }
}
