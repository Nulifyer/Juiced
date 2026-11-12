use super::{Manager, OptionsApply, Worker, WorkerState, publication};
use crate::{
    ApiError,
    config::{Policy, Provider},
    engines::models::{self, ModelFormat},
    execution_profile::ExecutionProfile,
    profiles::LaunchOptions,
    variants,
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};
use tokio::sync::OwnedMutexGuard;

const MAXIMUM_TARGETS: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SyncField {
    ContextTokens,
    Threads,
    GpuLayers,
    ParallelSlots,
}

const FIELDS: [SyncField; 4] = [
    SyncField::ContextTokens,
    SyncField::Threads,
    SyncField::GpuLayers,
    SyncField::ParallelSlots,
];

impl SyncField {
    fn get(self, options: &LaunchOptions) -> Option<u32> {
        match self {
            Self::ContextTokens => options.context_tokens,
            Self::Threads => options.threads,
            Self::GpuLayers => options.gpu_layers,
            Self::ParallelSlots => options.parallel_slots,
        }
    }

    fn set(self, options: &mut LaunchOptions, value: Option<u32>) {
        match self {
            Self::ContextTokens => options.context_tokens = value,
            Self::Threads => options.threads = value,
            Self::GpuLayers => options.gpu_layers = value,
            Self::ParallelSlots => options.parallel_slots = value,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncTarget {
    model: String,
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantSync {
    expected_revision: u64,
    targets: Vec<SyncTarget>,
    #[serde(default)]
    fields: Option<Vec<SyncField>>,
    #[serde(default)]
    apply: OptionsApply,
}

struct Entry {
    id: String,
    path: PathBuf,
    adapter: Provider,
    native: bool,
    saved: LaunchOptions,
    revision: u64,
    max_inflight: u32,
}

#[derive(Default)]
struct Facts {
    architecture: Option<String>,
    block_count: Option<u64>,
    training_context: Option<u64>,
    format: Option<ModelFormat>,
    bytes: u64,
}

fn inspect(path: PathBuf) -> Facts {
    let Ok((format, bytes)) = models::detect(&path) else {
        return Facts::default();
    };
    let (architecture, block_count, training_context) = match &format {
        ModelFormat::Gguf { declared, shape } => (
            declared.architecture.clone(),
            shape.block_count,
            declared.training_context_tokens,
        ),
        _ => (None, None, None),
    };
    Facts {
        architecture,
        block_count,
        training_context,
        format: Some(format),
        bytes,
    }
}

async fn inspect_all(paths: Vec<(String, PathBuf)>) -> BTreeMap<String, Facts> {
    tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|(id, path)| (id, inspect(path)))
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn ineligible(
    source: &Entry,
    source_facts: &Facts,
    target: &Entry,
    facts: &Facts,
) -> Option<&'static str> {
    if source.adapter != Provider::LlamaCpp {
        return Some("typed_options_unsupported");
    }
    if target.adapter != source.adapter {
        return Some("different_engine");
    }
    if target.native {
        return Some("native_settings_authority");
    }
    fn differs<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
        left.is_some() && right.is_some() && left != right
    }
    if differs(&source_facts.architecture, &facts.architecture)
        || differs(&source_facts.block_count, &facts.block_count)
    {
        return Some("different_architecture");
    }
    None
}

fn field_names(
    fields: &[SyncField],
    options: &LaunchOptions,
    other: &LaunchOptions,
) -> Vec<SyncField> {
    fields
        .iter()
        .copied()
        .filter(|field| field.get(options) != field.get(other))
        .collect()
}

fn invalid(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "revision_conflict",
        "A model changed. Refresh before copying settings; nothing was saved.",
    )
}

struct Planned {
    worker: Arc<Worker>,
    state: OwnedMutexGuard<WorkerState>,
    profile: ExecutionProfile,
    restart: bool,
    report: usize,
}

impl Manager {
    async fn variant_entries(&self) -> (Vec<Entry>, u64) {
        let config = self.config.lock().await;
        let parent = self.config_path.parent().expect("configuration parent");
        let entries = config
            .models
            .iter()
            .map(|spec| Entry {
                id: spec.id.clone(),
                path: if spec.model_file.is_relative() {
                    parent.join(&spec.model_file)
                } else {
                    spec.model_file.clone()
                },
                adapter: spec.execution_profile.adapter,
                native: spec.execution_profile.native_model_id().is_some(),
                saved: spec
                    .execution_profile
                    .options()
                    .cloned()
                    .unwrap_or_default(),
                revision: spec.revision,
                max_inflight: spec.max_inflight,
            })
            .collect();
        (entries, config.resident_budget_bytes)
    }

    async fn variant_group(
        &self,
        id: &str,
    ) -> Result<(Vec<Entry>, u64, Option<variants::Group>), ApiError> {
        self.worker(id)?;
        let (entries, budget) = self.variant_entries().await;
        if !entries.iter().any(|entry| entry.id == id) {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_model",
                "The model is not registered.",
            ));
        }
        let group = variants::groups(
            entries
                .iter()
                .map(|entry| (entry.id.as_str(), entry.path.as_path())),
        )
        .into_iter()
        .find(|group| group.members.iter().any(|member| member == id));
        Ok((entries, budget, group))
    }

    pub async fn variants(&self, id: &str) -> Result<Value, ApiError> {
        let (entries, _, group) = self.variant_group(id).await?;
        let by_id: BTreeMap<&str, &Entry> = entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let source = by_id[id];
        let syncable = source.adapter == Provider::LlamaCpp;
        let mut view = json!({
            "model": id,
            "revision": source.revision,
            "provider": source.adapter,
            "syncable": syncable,
            "saved": source.saved,
            "fields": FIELDS,
            "group": null,
            "members": [],
        });
        let Some(group) = group else {
            return Ok(view);
        };
        let facts = inspect_all(
            group
                .members
                .iter()
                .map(|member| (member.clone(), by_id[member.as_str()].path.clone()))
                .collect(),
        )
        .await;
        let source_facts = &facts[id];
        let members: Vec<Value> = group
            .members
            .iter()
            .filter(|member| *member != id)
            .map(|member| {
                let target = by_id[member.as_str()];
                let observation = self.worker(member).ok().and_then(|worker| {
                    worker
                        .observation
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                });
                let reason = ineligible(source, source_facts, target, &facts[member.as_str()]);
                json!({
                    "model": member,
                    "revision": target.revision,
                    "quant": variants::classify(member).quant,
                    "provider": target.adapter,
                    "policy": observation.as_ref().map(|view| view.policy),
                    "state": observation.as_ref().map(|view| view.state),
                    "loaded": observation.as_ref().is_some_and(|view| view.loaded),
                    "saved": target.saved,
                    "eligible": reason.is_none(),
                    "reason": reason,
                    "differs": field_names(&FIELDS, &source.saved, &target.saved),
                })
            })
            .collect();
        view["group"] = json!({"id": group.id, "label": group.label});
        view["members"] = Value::Array(members);
        Ok(view)
    }

    pub async fn sync_variants(
        self: &Arc<Self>,
        id: &str,
        request: VariantSync,
    ) -> Result<Value, ApiError> {
        let mut fields: Vec<SyncField> = request.fields.unwrap_or_else(|| FIELDS.to_vec());
        fields.sort();
        fields.dedup();
        if fields.is_empty() {
            return Err(invalid(
                "invalid_sync_fields",
                "Choose at least one setting to copy.",
            ));
        }
        if request.targets.is_empty() || request.targets.len() > MAXIMUM_TARGETS {
            return Err(invalid(
                "invalid_sync_targets",
                "Choose 1 to 64 other quants of this model.",
            ));
        }
        let mut targets: BTreeMap<String, u64> = BTreeMap::new();
        for target in request.targets {
            if target.model == id
                || targets
                    .insert(target.model, target.expected_revision)
                    .is_some()
            {
                return Err(invalid(
                    "invalid_sync_targets",
                    "Each target must be a different quant, listed once.",
                ));
            }
        }
        let (entries, budget, group) = self.variant_group(id).await?;
        let by_id: BTreeMap<&str, &Entry> = entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let source = by_id[id];
        if source.revision != request.expected_revision {
            return Err(conflict());
        }
        if source.adapter != Provider::LlamaCpp {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "typed_options_unsupported",
                "Only llama.cpp launch settings can be copied.",
            ));
        }
        let members: BTreeSet<&str> = group
            .iter()
            .flat_map(|group| group.members.iter().map(String::as_str))
            .collect();
        if let Some(stranger) = targets
            .keys()
            .find(|target| !members.contains(target.as_str()))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "not_a_variant",
                format!("{stranger} is not another quant of this model."),
            ));
        }
        let facts = inspect_all(
            std::iter::once(id)
                .chain(targets.keys().map(String::as_str))
                .map(|member| (member.to_owned(), by_id[member].path.clone()))
                .collect(),
        )
        .await;
        let mut locked = Vec::with_capacity(targets.len());
        for (target, expected) in &targets {
            let worker = self.worker(target)?;
            let state = worker.state.clone().lock_owned().await;
            self.ensure_registered(&worker)?;
            if state.revision != *expected || by_id[target.as_str()].revision != *expected {
                return Err(conflict());
            }
            locked.push((worker, state));
        }
        let mut reports = Vec::with_capacity(locked.len());
        let mut planned = Vec::new();
        for (worker, mut state) in locked {
            let target = by_id[worker.spec.id.as_str()];
            let target_facts = &facts[target.id.as_str()];
            let mut report = json!({
                "model": target.id,
                "outcome": "skipped",
                "reason": null,
                "revision": state.revision,
                "copied": [],
                "skipped_fields": [],
                "restarted": false,
            });
            let busy = if let Some(reason) = ineligible(source, &facts[id], target, target_facts) {
                Some(reason)
            } else {
                match request.apply {
                    OptionsApply::NextLoad => (state.running.is_some()
                        || worker.preparing.load(Ordering::Acquire)
                        || worker.active.load(Ordering::Acquire) != 0
                        || self.processes.snapshot(&target.id).is_some())
                    .then_some("worker_in_use"),
                    OptionsApply::Reload => {
                        Self::reap(&mut state);
                        if worker.active.load(Ordering::Acquire) != 0
                            || worker.preparing.load(Ordering::Acquire)
                        {
                            Some("worker_busy")
                        } else {
                            Self::process_available(&worker, state.running.is_some())
                                .err()
                                .map(|error| error.code)
                        }
                    }
                }
            };
            if let Some(reason) = busy {
                report["reason"] = json!(reason);
                reports.push(report);
                continue;
            }
            let mut next = target.saved.clone();
            let mut copied = Vec::new();
            let mut skipped = Vec::new();
            for field in &fields {
                let value = field.get(&source.saved);
                if value == field.get(&target.saved) {
                    continue;
                }
                let refusal = match (field, value) {
                    (SyncField::ParallelSlots, Some(slots)) if slots != target.max_inflight => {
                        Some(json!({
                            "field": field, "reason": "slot_resize_requires_registration",
                            "value": slots, "limit": target.max_inflight,
                        }))
                    }
                    (SyncField::ContextTokens, Some(context)) => {
                        let context = u64::from(context);
                        if let Some(limit) = target_facts
                            .training_context
                            .filter(|limit| context > *limit)
                        {
                            Some(
                                json!({"field": field, "reason": "context_exceeds_training", "value": context, "limit": limit}),
                            )
                        } else {
                            target_facts
                                .format
                                .as_ref()
                                .map(|format| models::estimate(format, target_facts.bytes, Some(context)))
                                .filter(|estimate| estimate.basis == "gguf-shape" && estimate.resident_bytes > budget)
                                .map(|estimate| json!({
                                    "field": field, "reason": "context_exceeds_budget", "value": context,
                                    "estimated_bytes": estimate.resident_bytes, "limit": budget,
                                }))
                        }
                    }
                    _ => None,
                };
                match refusal {
                    Some(refusal) => skipped.push(refusal),
                    None => {
                        field.set(&mut next, value);
                        copied.push(*field);
                    }
                }
            }
            report["copied"] = json!(copied);
            report["skipped_fields"] = json!(skipped);
            if copied.is_empty() {
                report["outcome"] = json!(if skipped.is_empty() {
                    "unchanged"
                } else {
                    "skipped"
                });
                reports.push(report);
                continue;
            }
            let mut profile = worker.spec.execution_profile.clone();
            let resolution = profile
                .set_local_options(next)
                .map_err(|_| "native_settings_authority")
                .and_then(|()| profile.resolved_arguments().map_err(|error| error.code))
                .and_then(|(_, applied)| {
                    applied
                        .and_then(|applied| applied.parallel_slots)
                        .is_none_or(|slots| slots == target.max_inflight)
                        .then_some(())
                        .ok_or("slot_resize_requires_registration")
                });
            if let Err(reason) = resolution {
                report["reason"] = json!(reason);
                report["copied"] = json!([]);
                reports.push(report);
                continue;
            }
            let restart = request.apply == OptionsApply::Reload
                && state.policy != Policy::Disabled
                && (state.running.is_some() || state.policy == Policy::Hot);
            report["outcome"] = json!("updated");
            planned.push(Planned {
                worker,
                state,
                profile,
                restart,
                report: reports.len(),
            });
            reports.push(report);
        }
        let mut generation = None;
        if !planned.is_empty() {
            let restarts: Vec<(usize, bool)> = planned
                .iter()
                .map(|plan| (plan.report, plan.restart))
                .collect();
            let previous: Vec<ExecutionProfile> = planned
                .iter()
                .map(|plan| plan.worker.spec.execution_profile.clone())
                .collect();
            let (replacements, committed) = self
                .replace_profiles(
                    planned
                        .into_iter()
                        .map(|plan| (plan.worker, plan.state, plan.profile))
                        .collect(),
                )
                .await?;
            generation = Some(committed);
            for ((replacement, (report, restart)), previous) in
                replacements.into_iter().zip(restarts).zip(previous)
            {
                reports[report]["revision"] = json!(committed);
                if !restart {
                    continue;
                }
                match self.restart_worker(&replacement).await {
                    Ok(()) => reports[report]["restarted"] = json!(true),
                    Err(error) => {
                        let (restored, restarted) =
                            self.restore_profile(replacement, previous).await;
                        let entry = &mut reports[report];
                        entry["outcome"] = json!("reload_failed");
                        entry["reason"] = json!(error.code);
                        entry["restored_revision"] = json!(restored);
                        entry["restarted"] = json!(restarted);
                        if let Some(revision) = restored {
                            entry["revision"] = json!(revision);
                        }
                    }
                }
            }
        }
        Ok(json!({
            "model": id,
            "revision": source.revision,
            "apply": match request.apply {
                OptionsApply::NextLoad => "next_load",
                OptionsApply::Reload => "reload",
            },
            "fields": fields,
            "generation": generation,
            "results": reports,
        }))
    }

    async fn restore_profile(
        self: &Arc<Self>,
        failed: Arc<Worker>,
        previous: ExecutionProfile,
    ) -> (Option<u64>, bool) {
        let state = failed.state.clone().lock_owned().await;
        if failed.active.load(Ordering::Acquire) != 0 || failed.preparing.load(Ordering::Acquire) {
            return (None, false);
        }
        match self.replace_profiles(vec![(failed, state, previous)]).await {
            Ok((mut workers, revision)) => {
                let worker = workers.pop().expect("restored worker");
                (Some(revision), self.restart_worker(&worker).await.is_ok())
            }
            Err(_) => (None, false),
        }
    }

    /// Commits every profile change in one configuration generation.
    async fn replace_profiles(
        self: &Arc<Self>,
        changes: Vec<(Arc<Worker>, OwnedMutexGuard<WorkerState>, ExecutionProfile)>,
    ) -> Result<(Vec<Arc<Worker>>, u64), ApiError> {
        for (worker, _, _) in &changes {
            self.ensure_registered(worker)?;
        }
        let config = self.config.clone().lock_owned().await;
        let mut next = config.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration revision cannot advance.",
            )
        })?;
        let generation = next.generation;
        for (worker, _, profile) in &changes {
            let spec = next
                .models
                .iter_mut()
                .find(|model| model.id == worker.spec.id)
                .expect("registered model");
            spec.execution_profile = profile.clone();
            spec.revision = generation;
        }
        next.validate().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_profile",
                "The saved launch profile is invalid.",
            )
        })?;
        let next = Self::check_config_size(next).await?;
        let parent = self.config_path.parent().expect("configuration parent");
        let mut entries = Vec::with_capacity(changes.len());
        for (worker, state, _) in changes {
            let resolved = next
                .models
                .iter()
                .find(|model| model.id == worker.spec.id)
                .expect("registered model")
                .resolved(parent);
            let resolved = self.bind_runtime(resolved).await?;
            let replacement = Self::create_worker(
                resolved,
                worker._artifact.clone(),
                &self.admission,
                &self.processes,
                Some(worker.slots.clone()),
            )?;
            entries.push((worker, state, replacement));
        }
        let mut stopped = Vec::new();
        for (worker, state, _) in &mut entries {
            if let Some(run) = state.running.as_mut() {
                if let Err(error) = run.child.stop().await {
                    state.error = Some("worker_cleanup_pending");
                    Self::publish(worker, state);
                    drop(entries);
                    for worker in stopped {
                        let _ = self.restart_worker(&worker).await;
                    }
                    return Err(super::process_error(error));
                }
                state.running = None;
                state.error = None;
                Self::publish(worker, state);
                stopped.push(worker.clone());
            }
        }
        let replacements: Vec<Arc<Worker>> = entries
            .iter()
            .map(|(_, _, replacement)| replacement.clone())
            .collect();
        let outcome = match self
            .publish_configuration(config, next, publication::Effect::ReplaceMany { entries })
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                for worker in stopped {
                    let _ = self.restart_worker(&worker).await;
                }
                return Err(error);
            }
        };
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Settings committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        Ok((replacements, generation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn manager() -> (tempfile::TempDir, Arc<Manager>) {
        let directory = tempfile::tempdir().unwrap();
        let model = |id: &str, policy: &str| {
            let file = directory.path().join(format!("{id}.gguf"));
            std::fs::write(&file, b"quant sync test weights").unwrap();
            json!({"id":id,"model_file":file,"provider":"llama-cpp","command":"unstarted-engine",
                "args":["--model","{model}","--port","{port}"],"resident_bytes":1024,"max_inflight":1,
                "operations":["completions"],"policy":policy,"revision":1})
        };
        let config: Config = serde_json::from_value(json!({
            "schema_version":1,"inhibit_suspend":false,"resident_budget_bytes":4096,
            "startup_timeout_ms":1000,"request_timeout_ms":1000,
            "models":[model("Demo-GGUF-Q4_K_M", "on-demand"), model("Demo-GGUF-Q8_0", "hot"), model("Demo-GGUF-Q6_K", "on-demand")]
        }))
        .unwrap();
        let manager =
            Manager::assemble(config, directory.path().join("config.json"), None, false).unwrap();
        (directory, manager)
    }

    fn request(targets: &[&str], apply: OptionsApply) -> VariantSync {
        VariantSync {
            expected_revision: 1,
            targets: targets
                .iter()
                .map(|model| SyncTarget {
                    model: (*model).into(),
                    expected_revision: 1,
                })
                .collect(),
            fields: None,
            apply,
        }
    }

    #[tokio::test]
    async fn failed_reload_restores_only_the_failed_target_and_keeps_the_rest() {
        let (_directory, manager) = manager();
        let reply = manager
            .sync_variants(
                "Demo-GGUF-Q4_K_M",
                request(&["Demo-GGUF-Q8_0", "Demo-GGUF-Q6_K"], OptionsApply::Reload),
            )
            .await
            .unwrap();
        assert_eq!(reply["generation"], Value::Null);
        let saved = manager
            .set_options(
                "Demo-GGUF-Q4_K_M",
                1,
                LaunchOptions {
                    threads: Some(3),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut sync = request(&["Demo-GGUF-Q8_0", "Demo-GGUF-Q6_K"], OptionsApply::Reload);
        sync.expected_revision = saved["revision"].as_u64().unwrap();
        let reply = manager
            .sync_variants("Demo-GGUF-Q4_K_M", sync)
            .await
            .unwrap();
        let generation = reply["generation"].as_u64().unwrap();
        let results = reply["results"].as_array().unwrap();
        let hot = &results[1];
        assert_eq!(hot["model"], "Demo-GGUF-Q8_0");
        assert_eq!(hot["outcome"], "reload_failed", "{reply}");
        assert_eq!(hot["restarted"], false);
        let restored = hot["restored_revision"].as_u64().unwrap();
        assert!(restored > generation);
        assert_eq!(hot["revision"], restored);
        let plain = &results[0];
        assert_eq!(plain["model"], "Demo-GGUF-Q6_K");
        assert_eq!(plain["outcome"], "updated");
        assert_eq!(plain["restarted"], false);
        assert_eq!(plain["revision"], generation);
        let persisted = Config::read(&manager.config_path).unwrap();
        let options = |id: &str| {
            persisted
                .models
                .iter()
                .find(|model| model.id == id)
                .unwrap()
                .execution_profile
                .options()
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(options("Demo-GGUF-Q6_K").threads, Some(3));
        assert_eq!(options("Demo-GGUF-Q8_0").threads, None);
        assert_eq!(
            persisted
                .models
                .iter()
                .find(|model| model.id == "Demo-GGUF-Q8_0")
                .unwrap()
                .policy,
            Policy::Hot
        );
    }
}
