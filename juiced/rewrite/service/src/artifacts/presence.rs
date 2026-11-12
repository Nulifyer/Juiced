use super::*;

const JOBS_PER_DIGEST: usize = 4;

#[derive(Clone, Debug, Serialize)]
pub struct PresenceJob {
    pub id: String,
    pub revision: u64,
    pub state: JobState,
    pub downloaded_bytes: u64,
    pub expected_bytes: Option<u64>,
    pub writer_active: bool,
}

#[derive(Debug, Default)]
pub struct Presence {
    pub artifacts: BTreeMap<String, u64>,
    pub jobs: BTreeMap<String, Vec<PresenceJob>>,
    pub sets: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct SourceRecord {
    pub job: String,
    pub source_url: String,
    pub sha256: String,
    pub bytes: u64,
}

impl ArtifactStore {
    pub fn presence(&self, digests: &BTreeSet<String>) -> Result<Presence, ArtifactError> {
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut presence = Presence::default();
        if digests.is_empty() {
            return Ok(presence);
        }
        for digest in digests {
            if let Some(artifact) = runtime
                .manifest
                .artifacts
                .get(digest)
                .filter(|artifact| !artifact.removing)
            {
                presence.artifacts.insert(digest.clone(), artifact.bytes);
            }
        }
        for (id, job) in &runtime.manifest.jobs {
            if !digests.contains(&job.expected_sha256) {
                continue;
            }
            let jobs = presence
                .jobs
                .entry(job.expected_sha256.clone())
                .or_default();
            if jobs.len() >= JOBS_PER_DIGEST {
                continue;
            }
            let view = job_view(id, job, &runtime);
            jobs.push(PresenceJob {
                id: view.id,
                revision: view.revision,
                state: view.state,
                downloaded_bytes: view.downloaded_bytes,
                expected_bytes: view.expected_bytes,
                writer_active: view.writer_active,
            });
        }
        for (id, record) in &runtime.manifest.artifact_sets {
            for member in &record.specification.members {
                if digests.contains(&member.artifact) {
                    let sets = presence.sets.entry(member.artifact.clone()).or_default();
                    if sets.last() != Some(id) {
                        sets.push(id.clone());
                    }
                }
            }
        }
        Ok(presence)
    }

    pub fn active_work_full(&self) -> bool {
        self.runtime
            .lock()
            .map(|runtime| runtime.active_work() >= self.limits.maximum_active_jobs)
            .unwrap_or(false)
    }

    pub fn completed_sources(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<SourceRecord>, ArtifactError> {
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut records = Vec::new();
        for (id, job) in &runtime.manifest.jobs {
            if records.len() >= limit {
                break;
            }
            if job.state != JobState::Complete {
                continue;
            }
            let (Some(url), Some(artifact)) = (&job.source_url, &job.artifact) else {
                continue;
            };
            let Some(record) = runtime
                .manifest
                .artifacts
                .get(artifact)
                .filter(|record| !record.removing)
            else {
                continue;
            };
            if url.starts_with(prefix) {
                records.push(SourceRecord {
                    job: id.clone(),
                    source_url: url.clone(),
                    sha256: record.sha256.clone(),
                    bytes: record.bytes,
                });
            }
        }
        Ok(records)
    }

    pub fn local_listing(&self, limit: usize) -> Result<StoreListing, ArtifactError> {
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut sources: BTreeMap<&str, (Option<&str>, Option<&str>)> = BTreeMap::new();
        for job in runtime.manifest.jobs.values() {
            if let Some(artifact) = job.artifact.as_deref() {
                sources.insert(
                    artifact,
                    (job.source_url.as_deref(), job.source_name.as_deref()),
                );
            }
        }
        let files = runtime
            .manifest
            .artifacts
            .iter()
            .take(limit)
            .map(|(id, artifact)| {
                let (url, name) = sources.get(id.as_str()).copied().unwrap_or_default();
                StoredFile {
                    id: id.clone(),
                    bytes: artifact.bytes,
                    path: self.artifact_path(id),
                    removing: artifact.removing,
                    references: artifact.references.iter().cloned().collect(),
                    active_holds: artifact.active_holds(),
                    source_url: url.map(str::to_owned),
                    source_name: name.map(str::to_owned),
                }
            })
            .collect();
        let sets = runtime
            .manifest
            .artifact_sets
            .iter()
            .take(limit)
            .map(|(id, record)| StoredSet {
                id: id.clone(),
                specification: record.specification.clone(),
                in_use: record.in_use(),
                references: record.references().cloned().collect(),
            })
            .collect();
        Ok(StoreListing {
            generation: runtime.manifest.generation,
            truncated: runtime.manifest.artifacts.len() > limit
                || runtime.manifest.artifact_sets.len() > limit,
            files,
            sets,
        })
    }
}

#[derive(Clone, Debug)]
pub struct StoredFile {
    pub id: String,
    pub bytes: u64,
    pub path: PathBuf,
    pub removing: bool,
    pub references: Vec<String>,
    pub active_holds: usize,
    pub source_url: Option<String>,
    pub source_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StoredSet {
    pub id: String,
    pub specification: Arc<ArtifactSetSpec>,
    pub in_use: bool,
    pub references: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StoreListing {
    pub generation: u64,
    pub truncated: bool,
    pub files: Vec<StoredFile>,
    pub sets: Vec<StoredSet>,
}
