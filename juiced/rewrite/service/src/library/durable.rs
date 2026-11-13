use super::{
    Library, Part, PartStatus, PlannedSet, PullRecord, SetState, existing_status, observe,
};
use crate::{
    acquisitions::{Journal, Section},
    artifacts::{ArtifactSetSpec, ArtifactStore, DownloadRequest, JobState},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SavedPart {
    file: String,
    name: Option<String>,
    source_url: String,
    sha256: String,
    bytes: Option<u64>,
    maximum_bytes: u64,
    job: Option<String>,
    failed: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SavedSet {
    id: String,
    specification: ArtifactSetSpec,
    state: String,
    error: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SavedPull {
    id: String,
    key: String,
    repo: String,
    revision: String,
    label: Option<String>,
    parts: Vec<SavedPart>,
    set: Option<SavedSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<String>,
}

/// Failure codes are `&'static str` throughout the pull model; journal codes outside the known
/// set collapse to a generic one rather than leaking allocations.
fn code(value: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "artifact_job_failed",
        "artifact_job_cancelled",
        "artifact_job_quarantined",
        "artifact_job_missing",
        "artifact_operation_interrupted",
        "artifact_invalid",
        "artifact_not_found",
        "artifact_revision_conflict",
        "artifact_busy",
        "artifact_limit",
        "artifact_offline",
        "artifact_source_policy",
        "artifact_source_changed",
        "artifact_network",
        "artifact_hash_mismatch",
        "artifact_size_mismatch",
        "artifact_corrupt",
        "artifact_interrupted",
        "artifact_deadline",
        "artifact_set_binding_mismatch",
        "artifact_set_execution_unsupported",
        "storage_root_unavailable",
        "insufficient_storage",
        "artifact_io",
        "artifact_persistence",
    ];
    KNOWN
        .iter()
        .find(|known| **known == value)
        .copied()
        .unwrap_or("library_pull_failed")
}

fn saved(record: &PullRecord) -> SavedPull {
    SavedPull {
        id: record.id.clone(),
        key: record.key.clone(),
        repo: record.repo.clone(),
        revision: record.revision.clone(),
        label: record.label.clone(),
        parts: record
            .parts
            .iter()
            .map(|part| SavedPart {
                file: part.file.clone(),
                name: part.name.clone(),
                source_url: part.request.source_url.clone(),
                sha256: part.request.expected_sha256.clone(),
                bytes: part.request.expected_bytes,
                maximum_bytes: part.request.maximum_bytes,
                job: match &part.status {
                    PartStatus::Job { id, .. } => Some(id.clone()),
                    PartStatus::Present { job } => job.clone(),
                    _ => None,
                },
                failed: match part.status {
                    PartStatus::Failed(code) => Some(code.to_owned()),
                    _ => None,
                },
            })
            .collect(),
        set: record.set.as_ref().map(|set| {
            let (state, error) = match set.state {
                SetState::Pending => ("pending", None),
                SetState::Published => ("published", None),
                SetState::Failed(code) => ("failed", Some(code.to_owned())),
            };
            SavedSet {
                id: set.id.clone(),
                specification: set.specification.clone(),
                state: state.to_owned(),
                error,
            }
        }),
        checkpoint: record
            .checkpoint
            .and_then(|kind| serde_json::to_value(kind).ok())
            .and_then(|value| value.as_str().map(str::to_owned)),
    }
}

fn restored(saved: SavedPull, store: &ArtifactStore) -> PullRecord {
    let digests: BTreeSet<String> = saved.parts.iter().map(|part| part.sha256.clone()).collect();
    let presence = store.presence(&digests).ok();
    let parts = saved
        .parts
        .into_iter()
        .map(|part| {
            let status = if let Some(failed) = &part.failed {
                PartStatus::Failed(code(failed))
            } else if let Some(job) = &part.job
                && let Some(status) = observe(store, job, false, &part.sha256)
            {
                status
            } else if let Some(presence) = &presence {
                existing_status(presence, &part.sha256)
            } else {
                PartStatus::Queued
            };
            Part {
                file: part.file,
                name: part.name,
                request: DownloadRequest {
                    source_url: part.source_url,
                    expected_sha256: part.sha256,
                    expected_bytes: part.bytes,
                    maximum_bytes: part.maximum_bytes,
                },
                status,
            }
        })
        .collect();
    PullRecord {
        id: saved.id,
        key: saved.key,
        repo: saved.repo,
        revision: saved.revision,
        label: saved.label,
        parts,
        set: saved.set.map(|set| PlannedSet {
            id: set.id,
            specification: set.specification,
            state: match set.state.as_str() {
                "published" => SetState::Published,
                "failed" => SetState::Failed(code(set.error.as_deref().unwrap_or_default())),
                _ => SetState::Pending,
            },
        }),
        checkpoint: saved
            .checkpoint
            .and_then(|kind| serde_json::from_value(serde_json::Value::String(kind)).ok()),
    }
}

impl Library {
    pub fn attach_journal(&self, journal: Arc<Journal>) {
        let _ = self.journal.set(journal);
    }

    pub(crate) fn journal(&self) -> Option<Arc<Journal>> {
        self.journal.get().cloned()
    }

    pub(super) fn persist(&self, record: &PullRecord) {
        if let Some(journal) = self.journal.get()
            && let Ok(value) = serde_json::to_value(saved(record))
        {
            journal.put(Section::Pulls, &record.id, value);
        }
    }

    pub(super) fn forget_saved(&self, id: &str) {
        if let Some(journal) = self.journal.get() {
            journal.remove(Section::Pulls, id);
        }
    }

    /// Reloads journaled pulls after a restart. Transfers the crash interrupted resume from
    /// their committed bytes; transfers an operator paused stay paused.
    pub async fn recover(self: &Arc<Self>, store: Arc<ArtifactStore>) {
        let Some(journal) = self.journal.get().cloned() else {
            return;
        };
        let entries = journal.entries(Section::Pulls);
        let records = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || {
                entries
                    .into_iter()
                    .filter_map(|(id, value)| {
                        let saved = serde_json::from_value::<SavedPull>(value).ok();
                        if saved.is_none() {
                            journal.remove(Section::Pulls, &id);
                        }
                        saved
                    })
                    .map(|saved| {
                        let record = restored(saved, &store);
                        for part in &record.parts {
                            if let PartStatus::Job {
                                id,
                                state: JobState::Interrupted,
                                ..
                            } = &part.status
                                && let Ok(job) = store.job(id)
                            {
                                let _ = store.resume(id, job.revision);
                            }
                        }
                        record
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default()
        };
        for record in records {
            let terminal = record.terminal();
            self.persist(&record);
            let record = Arc::new(Mutex::new(record));
            self.pulls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(record.clone());
            if !terminal {
                let library = self.clone();
                let store = store.clone();
                tokio::spawn(async move { library.drive(store, record).await });
            }
        }
    }
}
