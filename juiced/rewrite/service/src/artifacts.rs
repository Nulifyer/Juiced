mod intake;
mod inventory;
mod presence;
#[cfg(test)]
mod set_tests;
mod sets;
pub use intake::{Intake, Published};
pub use inventory::{InventoryPage, InventoryRequest, Section as InventorySection};
pub use presence::{Presence, PresenceJob, SourceRecord};
pub mod inventory_bounds {
    pub use super::inventory::{
        DEFAULT_PAGE_SIZE, ERROR_PREVIEW_BYTES, MAXIMUM_PAGE_SIZE, REFERENCE_PREVIEW,
        RESPONSE_BYTES, RESPONSE_WORK_BYTES, SOURCE_URL_PREVIEW_BYTES,
    };
}
pub use sets::{
    ArtifactBinding, ArtifactRole, ArtifactSetLease, ArtifactSetMember, ArtifactSetOrigin,
    ArtifactSetSpec, ArtifactSetView, SetOperation,
};

use crate::storage::{AtomicWriteError, DirectoryLease, LiveRoots, same_file};
use axum::body::Bytes;
use reqwest::{StatusCode, Url, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const MANIFEST: &str = "artifacts.json";
const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourcePolicy {
    PublicHttps,
    AllowLoopbackHttp,
    Offline,
}

#[derive(Clone, Debug)]
pub struct ArtifactLimits {
    pub maximum_jobs: usize,
    pub maximum_active_jobs: usize,
    pub maximum_artifacts: usize,
    pub maximum_sets: usize,
    pub maximum_artifact_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_redirects: usize,
    pub maximum_manifest_bytes: u64,
    pub request_timeout: Duration,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            maximum_jobs: 1024,
            maximum_active_jobs: 2,
            maximum_artifacts: 16384,
            maximum_sets: 1024,
            maximum_artifact_bytes: 512 * 1024 * 1024 * 1024,
            maximum_total_bytes: 1024 * 1024 * 1024 * 1024,
            maximum_redirects: 5,
            maximum_manifest_bytes: 16 * 1024 * 1024,
            request_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadRequest {
    pub source_url: String,
    pub expected_sha256: String,
    pub expected_bytes: Option<u64>,
    pub maximum_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ImportRequest {
    pub expected_sha256: String,
    pub expected_bytes: Option<u64>,
    pub maximum_bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    Queued,
    Downloading,
    Pausing,
    Paused,
    Cancelling,
    Cancelled,
    Interrupted,
    Publishing,
    Quarantined,
    Failed,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Job {
    revision: u64,
    state: JobState,
    source_url: Option<String>,
    expected_sha256: String,
    expected_bytes: Option<u64>,
    maximum_bytes: u64,
    downloaded_bytes: u64,
    validator: Option<String>,
    artifact: Option<String>,
    error: Option<String>,
    restart: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    computed_digest: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    bytes: u64,
    sha256: String,
    references: BTreeSet<String>,
    removing: bool,
    #[serde(skip)]
    holds: Option<Arc<AtomicUsize>>,
}

impl Artifact {
    fn active_holds(&self) -> usize {
        hold_count(&self.holds)
    }
}

#[derive(Clone, Debug, Serialize)]
struct Manifest {
    schema_version: u32,
    generation: u64,
    jobs: BTreeMap<String, Job>,
    artifacts: BTreeMap<String, Artifact>,
    artifact_sets: BTreeMap<String, sets::SetRecord>,
}

struct UniqueMap<T>(BTreeMap<String, T>);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for UniqueMap<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
            type Value = UniqueMap<T>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a map with unique identifiers")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = input.next_entry::<String, T>()? {
                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom(
                            "Duplicate artifact metadata identifier",
                        ));
                    }
                }
                Ok(UniqueMap(entries))
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}
#[derive(Default)]
struct ManifestSets(Option<BTreeMap<String, sets::SetRecord>>);
impl<'de> Deserialize<'de> for ManifestSets {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        UniqueMap::deserialize(deserializer).map(|sets| Self(Some(sets.0)))
    }
}
impl<'de> Deserialize<'de> for Manifest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: u32,
            generation: u64,
            jobs: UniqueMap<Job>,
            artifacts: UniqueMap<Artifact>,
            #[serde(default)]
            artifact_sets: ManifestSets,
        }
        let wire = Wire::deserialize(deserializer)?;
        let artifact_sets = match (wire.schema_version, wire.artifact_sets.0) {
            (1, None) => BTreeMap::new(),
            (2, Some(sets)) => sets,
            _ => {
                return Err(serde::de::Error::custom(
                    "Unsupported or incomplete artifact manifest",
                ));
            }
        };
        Ok(Self {
            schema_version: 2,
            generation: wire.generation,
            jobs: wire.jobs.0,
            artifacts: wire.artifacts.0,
            artifact_sets,
        })
    }
}

fn hold_count(count: &Option<Arc<AtomicUsize>>) -> usize {
    count
        .as_ref()
        .map_or(0, |count| count.load(Ordering::Acquire))
}
struct Hold(Arc<AtomicUsize>);
impl Hold {
    fn acquire(count: &mut Option<Arc<AtomicUsize>>) -> Result<Self, ArtifactError> {
        let count = count.get_or_insert_with(|| Arc::new(AtomicUsize::new(0)));
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| ArtifactError::Limit)?;
        Ok(Self(count.clone()))
    }
}
impl Drop for Hold {
    fn drop(&mut self) {
        let previous = self.0.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

struct Control {
    command: AtomicU8,
    changed: Notify,
    finished: Notify,
    done: AtomicBool,
    storage_refused: AtomicBool,
}

impl Control {
    fn new() -> Self {
        Self {
            command: AtomicU8::new(RUN),
            changed: Notify::new(),
            finished: Notify::new(),
            done: AtomicBool::new(false),
            storage_refused: AtomicBool::new(false),
        }
    }

    async fn wait(&self) {
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

struct Runtime {
    manifest: Manifest,
    active: HashMap<String, Arc<Control>>,
    set_operations: Vec<std::sync::Weak<Control>>,
    failures: HashMap<String, &'static str>,
    quarantined_files: BTreeMap<String, u64>,
    instance: String,
    membership_epoch: u64,
}

impl Runtime {
    fn replace_manifest(&mut self, next: Manifest) {
        if inventory::membership_changed(&self.manifest, &next) {
            self.membership_epoch = self.membership_epoch.wrapping_add(1);
        }
        self.manifest = next;
    }
    fn active_work(&self) -> usize {
        self.active.len()
            + self
                .set_operations
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .filter(|control| !control.done.load(Ordering::Acquire))
                .count()
    }
}
struct SetWork(Arc<Control>);
impl Drop for SetWork {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::Release);
        self.0.finished.notify_waiters();
    }
}

struct TransferProgress {
    digest: Sha256,
    bytes: u64,
}

pub struct ArtifactStore {
    lease: DirectoryLease,
    scope: Arc<LiveRoots>,
    limits: ArtifactLimits,
    policy: SourcePolicy,
    runtime: Mutex<Runtime>,
    publication: Mutex<()>,
    stopping: AtomicBool,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobView {
    pub id: String,
    pub revision: u64,
    pub state: JobState,
    pub source_url: Option<String>,
    pub source_url_bytes: usize,
    pub source_url_truncated: bool,
    pub expected_sha256: String,
    pub expected_bytes: Option<u64>,
    pub maximum_bytes: u64,
    pub downloaded_bytes: u64,
    pub artifact: Option<String>,
    pub error: Option<String>,
    pub writer_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub computed_digest: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactView {
    pub id: String,
    pub bytes: u64,
    pub sha256: String,
    pub references: Vec<String>,
    pub active_holds: usize,
    pub removing: bool,
}

#[cfg(test)]
#[derive(Debug, Serialize)]
pub struct StoreInspection {
    pub schema_version: u32,
    pub generation: u64,
    pub source_policy: SourcePolicy,
    pub read_only: bool,
    pub recovery_authorized: bool,
    pub charged_bytes: u64,
    pub jobs: Vec<JobView>,
    pub artifacts: Vec<ArtifactView>,
    pub quarantined_files: Vec<QuarantinedFileView>,
    pub artifact_sets: Vec<ArtifactSetView>,
}

#[derive(Debug, Serialize)]
pub struct QuarantinedFileView {
    pub filename: String,
    pub reserved_bytes: u64,
    pub reason: &'static str,
}

pub struct ArtifactLease {
    _store: Arc<ArtifactStore>,
    _hold: Option<Hold>,
    set: Option<Arc<ArtifactSetLease>>,
    path: PathBuf,
    artifact: ArtifactView,
}

impl ArtifactLease {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn artifact(&self) -> &ArtifactView {
        &self.artifact
    }
    pub fn set_id(&self) -> Option<&str> {
        self.set.as_ref().map(|set| set.id())
    }
    pub fn companions(&self) -> Vec<(&str, ArtifactRole, &Path)> {
        let Some(set) = &self.set else {
            return Vec::new();
        };
        let specification = set.specification();
        specification
            .members
            .iter()
            .filter(|member| member.name != specification.primary)
            .filter_map(|member| {
                set.member(&member.name)
                    .map(|held| (member.name.as_str(), member.role, held.path()))
            })
            .collect()
    }
    pub(crate) fn verify_set(&self) -> Result<(), ArtifactError> {
        if let Some(set) = &self.set {
            set.verify()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ArtifactError {
    Invalid,
    NotFound,
    Conflict,
    Busy,
    Limit,
    Offline,
    SourcePolicy,
    SourceChanged,
    Network,
    HashMismatch,
    SizeMismatch,
    Corrupt,
    Stop,
    Deadline,
    SetBinding,
    SetUnsupported,
    RootUnavailable,
    Io(io::Error),
    Persistence(AtomicWriteError),
}

impl ArtifactError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "artifact_invalid",
            Self::NotFound => "artifact_not_found",
            Self::Conflict => "artifact_revision_conflict",
            Self::Busy => "artifact_busy",
            Self::Limit => "artifact_limit",
            Self::Offline => "artifact_offline",
            Self::SourcePolicy => "artifact_source_policy",
            Self::SourceChanged => "artifact_source_changed",
            Self::Network => "artifact_network",
            Self::HashMismatch => "artifact_hash_mismatch",
            Self::SizeMismatch => "artifact_size_mismatch",
            Self::Corrupt => "artifact_corrupt",
            Self::Stop => "artifact_interrupted",
            Self::Deadline => "artifact_deadline",
            Self::SetBinding => "artifact_set_binding_mismatch",
            Self::SetUnsupported => "artifact_set_execution_unsupported",
            Self::RootUnavailable => "storage_root_unavailable",
            Self::Io(_) => "artifact_io",
            Self::Persistence(_) => "artifact_persistence",
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::Invalid => {
                "Use a generated identifier, expected SHA256 and explicit bounded file size."
            }
            Self::NotFound => "The artifact or transfer is unavailable.",
            Self::Conflict => {
                "The artifact state changed. Refresh the current revision before retrying."
            }
            Self::Busy => "A model, reader or active operation still references this artifact.",
            Self::Limit => "The operation exceeds a configured artifact, job or byte limit.",
            Self::Offline => {
                "Network acquisition is disabled by offline policy. Import an already-authorized local file instead."
            }
            Self::SourcePolicy => {
                "Use a public HTTPS source without URL credentials, query secrets or fragments. Loopback HTTP requires explicit source policy."
            }
            Self::SourceChanged => {
                "The source did not honor the saved range validator. Retry from the beginning."
            }
            Self::Network => "The source request failed. Retry when the source is available.",
            Self::HashMismatch => {
                "The complete file does not match its expected SHA256. Retained bytes are quarantined."
            }
            Self::SizeMismatch => {
                "The file size does not match its declared bounds. Retained bytes are quarantined."
            }
            Self::Corrupt => "Artifact metadata or contained file identity failed validation.",
            Self::Stop => "The transfer was interrupted before publication.",
            Self::Deadline => {
                "Artifact verification exceeded its deadline. Preserved files remain available for inspection."
            }
            Self::SetBinding => "The declared model artifact does not match the set primary.",
            Self::SetUnsupported => {
                "This worker binding currently consumes one primary artifact. Companion contents remain stored without execution qualification."
            }
            Self::RootUnavailable => {
                "A retained storage root is unavailable. Restore its owned identity before retrying filesystem effects."
            }
            Self::Io(_) => {
                "The artifact storage operation failed. Inspect the managed storage location."
            }
            Self::Persistence(_) => {
                "The manifest operation did not establish full durability. Refresh the committed state before retrying."
            }
        }
    }
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.public_message())
    }
}
impl std::error::Error for ArtifactError {}
impl From<io::Error> for ArtifactError {
    fn from(error: io::Error) -> Self {
        if crate::storage::is_root_unavailable(&error) {
            Self::RootUnavailable
        } else {
            Self::Io(error)
        }
    }
}

impl From<AtomicWriteError> for ArtifactError {
    fn from(error: AtomicWriteError) -> Self {
        if error.root_unavailable() {
            Self::RootUnavailable
        } else {
            Self::Persistence(error)
        }
    }
}

impl ArtifactStore {
    pub fn open(
        lease: DirectoryLease,
        limits: ArtifactLimits,
        policy: SourcePolicy,
    ) -> Result<Arc<Self>, ArtifactError> {
        let scope = LiveRoots::single(lease.clone())?;
        Self::open_scoped(lease, limits, policy, scope)
    }

    pub fn open_scoped(
        lease: DirectoryLease,
        limits: ArtifactLimits,
        policy: SourcePolicy,
        scope: Arc<LiveRoots>,
    ) -> Result<Arc<Self>, ArtifactError> {
        if limits.maximum_jobs == 0
            || limits.maximum_active_jobs == 0
            || limits.maximum_artifacts == 0
            || limits.maximum_sets == 0
            || limits.maximum_artifact_bytes == 0
            || limits.maximum_total_bytes == 0
            || limits.maximum_manifest_bytes == 0
            || limits.request_timeout.is_zero()
        {
            return Err(ArtifactError::Invalid);
        }
        if !scope.contains(&lease) {
            return Err(ArtifactError::Invalid);
        }
        scope.verify().map_err(|_| ArtifactError::RootUnavailable)?;
        let path = lease.root().join(MANIFEST);
        let manifest = match regular(&path) {
            Ok(mut file) => {
                if file.metadata()?.len() > limits.maximum_manifest_bytes {
                    return Err(ArtifactError::Limit);
                }
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(limits.maximum_manifest_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > limits.maximum_manifest_bytes {
                    return Err(ArtifactError::Limit);
                }
                serde_json::from_slice::<Manifest>(&bytes).map_err(|_| ArtifactError::Corrupt)?
            }
            Err(ArtifactError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                let manifest = Manifest {
                    schema_version: 2,
                    generation: 0,
                    jobs: BTreeMap::new(),
                    artifacts: BTreeMap::new(),
                    artifact_sets: BTreeMap::new(),
                };
                manifest_within_limit(&manifest, limits.maximum_manifest_bytes)?;
                scope.write_json(&lease, MANIFEST, &manifest)?;
                manifest
            }
            Err(error) => return Err(error),
        };
        if manifest.schema_version != 2
            || manifest.jobs.len() > limits.maximum_jobs
            || manifest.artifacts.len() > limits.maximum_artifacts
        {
            return Err(ArtifactError::Corrupt);
        }
        for (id, job) in &manifest.jobs {
            job_id(id)?;
            validate_expected(
                &job.expected_sha256,
                job.expected_bytes,
                job.maximum_bytes,
                &limits,
            )?;
            if job.revision == 0 || job.downloaded_bytes > job.maximum_bytes {
                return Err(ArtifactError::Corrupt);
            }
            if let Some(url) = &job.source_url {
                source_url(url, SourcePolicy::AllowLoopbackHttp, false)?;
            }
        }
        for (id, artifact) in &manifest.artifacts {
            artifact_id(id)?;
            if artifact.sha256 != *id
                || artifact.bytes == 0
                || artifact.bytes > limits.maximum_artifact_bytes
                || artifact
                    .references
                    .iter()
                    .any(|reference| !reference_valid(reference))
            {
                return Err(ArtifactError::Corrupt);
            }
        }
        sets::validate_records(&manifest, &limits)?;
        lease.verify()?;
        let quarantined_files = orphan_files(&lease, &manifest, &limits)?;
        scope.verify().map_err(|_| ArtifactError::RootUnavailable)?;
        Ok(Arc::new(Self {
            lease,
            scope,
            limits,
            policy,
            runtime: Mutex::new(Runtime {
                manifest,
                active: HashMap::new(),
                set_operations: Vec::new(),
                failures: HashMap::new(),
                quarantined_files,
                instance: crate::storage::generated_id()?,
                membership_epoch: 0,
            }),
            publication: Mutex::new(()),
            stopping: AtomicBool::new(false),
        }))
    }

    pub fn generation(&self) -> u64 {
        self.runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .manifest
            .generation
    }
    pub fn source_policy(&self) -> SourcePolicy {
        self.policy
    }
    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub fn start_download(
        self: &Arc<Self>,
        request: DownloadRequest,
        expected_generation: u64,
    ) -> Result<JobView, ArtifactError> {
        if self.policy == SourcePolicy::Offline {
            return Err(ArtifactError::Offline);
        }
        let url = source_url(&request.source_url, self.policy, false)?;
        validate_expected(
            &request.expected_sha256,
            request.expected_bytes,
            request.maximum_bytes,
            &self.limits,
        )?;
        let (view, control) = self.begin(
            Some(url.to_string()),
            request.expected_sha256.to_ascii_lowercase(),
            request.expected_bytes,
            request.maximum_bytes,
            expected_generation,
        )?;
        let store = self.clone();
        let id = view.id.clone();
        tokio::spawn(async move {
            store.run(id, control, None).await;
        });
        Ok(view)
    }

    pub fn start_import(
        self: &Arc<Self>,
        file: File,
        request: ImportRequest,
        expected_generation: u64,
    ) -> Result<JobView, ArtifactError> {
        validate_expected(
            &request.expected_sha256,
            request.expected_bytes,
            request.maximum_bytes,
            &self.limits,
        )?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len() > request.maximum_bytes
            || request
                .expected_bytes
                .is_some_and(|bytes| bytes != metadata.len())
        {
            return Err(ArtifactError::SizeMismatch);
        }
        let (view, control) = self.begin(
            None,
            request.expected_sha256.to_ascii_lowercase(),
            request.expected_bytes,
            request.maximum_bytes,
            expected_generation,
        )?;
        let store = self.clone();
        let id = view.id.clone();
        tokio::spawn(async move {
            store.run(id, control, Some(file)).await;
        });
        Ok(view)
    }

    fn begin(
        &self,
        source_url: Option<String>,
        expected_sha256: String,
        expected_bytes: Option<u64>,
        maximum_bytes: u64,
        expected_generation: u64,
    ) -> Result<(JobView, Arc<Control>), ArtifactError> {
        self.begin_with(
            source_url,
            expected_sha256,
            expected_bytes,
            maximum_bytes,
            Some(expected_generation),
            None,
        )
    }

    fn begin_with(
        &self,
        source_url: Option<String>,
        expected_sha256: String,
        expected_bytes: Option<u64>,
        maximum_bytes: u64,
        expected_generation: Option<u64>,
        source_name: Option<String>,
    ) -> Result<(JobView, Arc<Control>), ArtifactError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if expected_generation.is_some_and(|generation| generation != runtime.manifest.generation) {
            return Err(ArtifactError::Conflict);
        }
        if runtime.manifest.jobs.len() >= self.limits.maximum_jobs
            || runtime.active_work() >= self.limits.maximum_active_jobs
            || runtime.manifest.artifacts.len() >= self.limits.maximum_artifacts
            || charged(&runtime)
                .checked_add(maximum_bytes)
                .is_none_or(|bytes| bytes > self.limits.maximum_total_bytes)
        {
            return Err(ArtifactError::Limit);
        }
        let job = Job {
            revision: 1,
            state: JobState::Queued,
            source_url,
            expected_sha256,
            expected_bytes,
            maximum_bytes,
            downloaded_bytes: 0,
            validator: None,
            artifact: None,
            error: None,
            restart: false,
            computed_digest: source_name.is_some(),
            source_name,
        };
        let id = self.reserve_intent(&mut runtime, job)?;
        if let Err(error) = self.create_staging(&id) {
            runtime.failures.insert(id, error.code());
            return Err(error);
        }
        let control = Arc::new(Control::new());
        runtime.active.insert(id.clone(), control.clone());
        Ok((
            job_view(
                &id,
                runtime.manifest.jobs.get(&id).expect("accepted job"),
                &runtime,
            ),
            control,
        ))
    }

    fn reserve_intent(&self, runtime: &mut Runtime, job: Job) -> Result<String, ArtifactError> {
        self.verify_mutation()?;
        let id = crate::storage::generated_id()?;
        if runtime.manifest.jobs.contains_key(&id)
            || runtime
                .quarantined_files
                .contains_key(&format!("job-{id}.part"))
        {
            return Err(ArtifactError::Conflict);
        }
        let mut next = runtime.manifest.clone();
        next.jobs.insert(id.clone(), job);
        let quarantine = next.clone();
        if let Err(error) = self.commit(runtime, next) {
            if matches!(
                error,
                ArtifactError::Persistence(_) | ArtifactError::RootUnavailable
            ) {
                if !runtime.manifest.jobs.contains_key(&id) {
                    let mut quarantine = quarantine;
                    let job = quarantine.jobs.get_mut(&id).expect("reserved intent");
                    job.state = JobState::Quarantined;
                    job.error = Some("artifact_intent_uncertain".into());
                    runtime.replace_manifest(quarantine);
                }
                runtime.failures.insert(id, "artifact_intent_uncertain");
            }
            return Err(error);
        }
        Ok(id)
    }

    pub fn pause(&self, id: &str, expected_revision: u64) -> Result<JobView, ArtifactError> {
        self.control(id, expected_revision, PAUSE)
    }
    pub fn cancel(&self, id: &str, expected_revision: u64) -> Result<JobView, ArtifactError> {
        self.control(id, expected_revision, CANCEL)
    }

    fn control(
        &self,
        id: &str,
        expected_revision: u64,
        command: u8,
    ) -> Result<JobView, ArtifactError> {
        job_id(id)?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let job = runtime
            .manifest
            .jobs
            .get(id)
            .ok_or(ArtifactError::NotFound)?;
        if job.revision != expected_revision {
            return Err(ArtifactError::Conflict);
        }
        if command == CANCEL
            && !runtime.active.contains_key(id)
            && matches!(
                job.state,
                JobState::Queued
                    | JobState::Downloading
                    | JobState::Pausing
                    | JobState::Cancelling
                    | JobState::Paused
                    | JobState::Interrupted
                    | JobState::Failed
                    | JobState::Quarantined
            )
        {
            let mut next = runtime.manifest.clone();
            let job = next.jobs.get_mut(id).expect("settled job");
            job.state = JobState::Cancelling;
            job.revision = job.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
            self.commit(&mut runtime, next)?;
            drop(runtime);
            self.cancel_finished(id)?;
            return self.job(id);
        }
        let control = runtime
            .active
            .get(id)
            .cloned()
            .ok_or(ArtifactError::Conflict)?;
        if !matches!(
            job.state,
            JobState::Queued | JobState::Downloading | JobState::Pausing
        ) {
            return Err(ArtifactError::Conflict);
        }
        let mut next = runtime.manifest.clone();
        let job = next.jobs.get_mut(id).expect("existing job");
        job.state = if command == PAUSE {
            JobState::Pausing
        } else {
            JobState::Cancelling
        };
        job.revision = job.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
        control.command.fetch_max(command, Ordering::AcqRel);
        control.changed.notify_one();
        let committed = self.commit(&mut runtime, next);
        if let Err(error) = &committed {
            runtime.failures.insert(id.to_owned(), error.code());
            if matches!(error, ArtifactError::RootUnavailable) {
                control.storage_refused.store(true, Ordering::Release);
            }
        }
        committed?;
        Ok(job_view(
            id,
            runtime.manifest.jobs.get(id).expect("updated job"),
            &runtime,
        ))
    }

    pub fn resume(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
    ) -> Result<JobView, ArtifactError> {
        self.restart(id, expected_revision, false, None)
    }
    pub fn retry(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
    ) -> Result<JobView, ArtifactError> {
        self.restart(id, expected_revision, true, None)
    }
    pub fn resume_import(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
        file: File,
    ) -> Result<JobView, ArtifactError> {
        self.restart(id, expected_revision, true, Some(file))
    }

    fn restart(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
        cold: bool,
        file: Option<File>,
    ) -> Result<JobView, ArtifactError> {
        job_id(id)?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let job = runtime
            .manifest
            .jobs
            .get(id)
            .ok_or(ArtifactError::NotFound)?;
        if job.revision != expected_revision {
            return Err(ArtifactError::Conflict);
        }
        if runtime.active.contains_key(id) {
            return Err(ArtifactError::Busy);
        }
        if runtime.active_work() >= self.limits.maximum_active_jobs {
            return Err(ArtifactError::Limit);
        }
        if job.state == JobState::Publishing {
            return Err(ArtifactError::Conflict);
        }
        if !matches!(
            effective_state(id, job, &runtime),
            JobState::Paused | JobState::Interrupted | JobState::Failed | JobState::Quarantined
        ) {
            return Err(ArtifactError::Conflict);
        }
        if job.source_url.is_some() {
            if self.policy == SourcePolicy::Offline {
                return Err(ArtifactError::Offline);
            }
            if file.is_some() {
                return Err(ArtifactError::Invalid);
            }
        } else if file.is_none() {
            return Err(ArtifactError::Invalid);
        }
        if let Some(file) = &file {
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.len() > job.maximum_bytes
                || job
                    .expected_bytes
                    .is_some_and(|bytes| bytes != metadata.len())
            {
                return Err(ArtifactError::SizeMismatch);
            }
        }
        let mut next = runtime.manifest.clone();
        let job = next.jobs.get_mut(id).expect("existing job");
        job.state = JobState::Queued;
        job.restart = cold;
        job.error = None;
        job.revision = job.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
        self.commit(&mut runtime, next)?;
        runtime.failures.remove(id);
        let control = Arc::new(Control::new());
        runtime.active.insert(id.to_owned(), control.clone());
        let view = job_view(
            id,
            runtime.manifest.jobs.get(id).expect("restarted job"),
            &runtime,
        );
        drop(runtime);
        let store = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            store.run(id, control, file).await;
        });
        Ok(view)
    }

    pub fn forget_job(&self, id: &str, expected_revision: u64) -> Result<(), ArtifactError> {
        job_id(id)?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if runtime.active.contains_key(id) {
            return Err(ArtifactError::Busy);
        }
        let job = runtime
            .manifest
            .jobs
            .get(id)
            .ok_or(ArtifactError::NotFound)?;
        if job.revision != expected_revision {
            return Err(ArtifactError::Conflict);
        }
        if !matches!(job.state, JobState::Complete | JobState::Cancelled) {
            return Err(ArtifactError::Conflict);
        }
        let mut next = runtime.manifest.clone();
        next.jobs.remove(id);
        self.commit(&mut runtime, next)?;
        runtime.failures.remove(id);
        Ok(())
    }

    pub fn job(&self, id: &str) -> Result<JobView, ArtifactError> {
        job_id(id)?;
        self.lease.verify()?;
        self.retained_job(id)
    }

    pub fn source_file_name(&self, artifact: &str) -> Option<String> {
        let runtime = self.runtime.lock().ok()?;
        runtime
            .manifest
            .jobs
            .values()
            .rev()
            .filter(|job| job.artifact.as_deref() == Some(artifact))
            .find_map(|job| {
                if let Some(name) = &job.source_name {
                    return Some(name.clone());
                }
                let url = Url::parse(job.source_url.as_deref()?).ok()?;
                url.path_segments()?
                    .next_back()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
            })
    }

    fn retained_job(&self, id: &str) -> Result<JobView, ArtifactError> {
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        Ok(job_view(
            id,
            runtime
                .manifest
                .jobs
                .get(id)
                .ok_or(ArtifactError::NotFound)?,
            &runtime,
        ))
    }

    #[cfg(test)]
    pub fn inspect(&self) -> Result<StoreInspection, ArtifactError> {
        self.lease.verify()?;
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let membership = sets::membership(&runtime.manifest);
        Ok(StoreInspection {
            schema_version: 1,
            generation: runtime.manifest.generation,
            source_policy: self.policy,
            read_only: true,
            recovery_authorized: false,
            charged_bytes: charged(&runtime),
            jobs: runtime
                .manifest
                .jobs
                .iter()
                .map(|(id, job)| job_view(id, job, &runtime))
                .collect(),
            artifacts: runtime
                .manifest
                .artifacts
                .iter()
                .map(|(id, artifact)| {
                    let mut view = artifact_view(id, artifact);
                    if let Some(sets) = membership.get(id.as_str()) {
                        view.references
                            .extend(sets.iter().map(|set| format!("artifact-set:{set}")));
                    }
                    view
                })
                .collect(),
            artifact_sets: sets::views(&runtime.manifest),
            quarantined_files: runtime
                .quarantined_files
                .iter()
                .map(|(filename, reserved_bytes)| QuarantinedFileView {
                    filename: filename.clone(),
                    reserved_bytes: *reserved_bytes,
                    reason: "unreferenced_generated_file",
                })
                .collect(),
        })
    }

    pub async fn wait(&self, id: &str) -> Result<JobView, ArtifactError> {
        job_id(id)?;
        let control = self
            .runtime
            .lock()
            .map_err(|_| ArtifactError::Corrupt)?
            .active
            .get(id)
            .cloned();
        if let Some(control) = control {
            control.wait().await;
        }
        self.retained_job(id)
    }

    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let controls: Vec<_> = {
            let runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            runtime
                .active
                .values()
                .cloned()
                .chain(
                    runtime
                        .set_operations
                        .iter()
                        .filter_map(std::sync::Weak::upgrade),
                )
                .collect()
        };
        for control in &controls {
            control.command.store(CANCEL, Ordering::Release);
            control.changed.notify_one();
        }
        for control in controls {
            control.wait().await;
        }
    }

    fn set_work(&self) -> Result<SetWork, ArtifactError> {
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        runtime.set_operations.retain(|control| {
            control
                .upgrade()
                .is_some_and(|control| !control.done.load(Ordering::Acquire))
        });
        if runtime.active_work() >= self.limits.maximum_active_jobs {
            return Err(ArtifactError::Limit);
        }
        let control = Arc::new(Control::new());
        runtime.set_operations.push(Arc::downgrade(&control));
        Ok(SetWork(control))
    }

    pub fn hold(self: &Arc<Self>, id: &str) -> Result<ArtifactLease, ArtifactError> {
        let held = self.reserve_lease(id)?;
        let (bytes, digest) = hash(&held.path, self.limits.maximum_artifact_bytes)?;
        if bytes != held.artifact.bytes || digest != held.artifact.sha256 {
            return Err(ArtifactError::Corrupt);
        }
        self.lease.verify()?;
        Ok(held)
    }

    fn reserve_lease(self: &Arc<Self>, id: &str) -> Result<ArtifactLease, ArtifactError> {
        artifact_id(id)?;
        self.lease.verify()?;
        let (artifact, hold) = {
            let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
            let artifact = runtime
                .manifest
                .artifacts
                .get_mut(id)
                .filter(|artifact| !artifact.removing)
                .ok_or(ArtifactError::NotFound)?;
            let hold = Hold::acquire(&mut artifact.holds)?;
            (artifact_view(id, artifact), hold)
        };
        Ok(ArtifactLease {
            _store: self.clone(),
            _hold: Some(hold),
            set: None,
            path: self.artifact_path(id),
            artifact,
        })
    }

    pub fn reference(
        &self,
        id: &str,
        reference: &str,
        add: bool,
        expected_generation: u64,
    ) -> Result<ArtifactView, ArtifactError> {
        self.reference_binding(id, None, reference, add, expected_generation)
    }

    pub fn remove(&self, id: &str, expected_generation: u64) -> Result<(), ArtifactError> {
        artifact_id(id)?;
        {
            let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
            if runtime.manifest.generation != expected_generation {
                return Err(ArtifactError::Conflict);
            }
            let artifact = runtime
                .manifest
                .artifacts
                .get(id)
                .ok_or(ArtifactError::NotFound)?;
            if artifact.removing
                || sets::contains(&runtime.manifest, id)
                || !artifact.references.is_empty()
                || artifact.active_holds() > 0
            {
                return Err(ArtifactError::Busy);
            }
            let mut next = runtime.manifest.clone();
            next.artifacts
                .get_mut(id)
                .expect("existing artifact")
                .removing = true;
            self.commit(&mut runtime, next)?;
        }
        self.verify_mutation()?;
        regular(&self.artifact_path(id))?;
        self.verify_mutation()?;
        fs::remove_file(self.artifact_path(id))?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut next = runtime.manifest.clone();
        next.artifacts.remove(id);
        self.commit(&mut runtime, next)
    }

    async fn run(self: Arc<Self>, id: String, control: Arc<Control>, import: Option<File>) {
        let result = self.transfer(&id, &control, import).await;
        let result = if control.storage_refused.load(Ordering::Acquire) {
            Err(ArtifactError::RootUnavailable)
        } else {
            result
        };
        let failure = result.as_ref().err().map(ArtifactError::code);
        let command = control.command.load(Ordering::Acquire);
        let store = self.clone();
        let settled_id = id.clone();
        let settled = tokio::task::spawn_blocking(move || {
            store.settle_transfer(&settled_id, command, result)
        })
        .await;
        if let Ok(mut runtime) = self.runtime.lock() {
            let failure = match &settled {
                Ok(Err(error)) => Some(error.code()),
                Err(_) => Some("artifact_settlement_interrupted"),
                Ok(Ok(())) => failure,
            };
            if let Some(failure) = failure {
                runtime.failures.insert(id.clone(), failure);
            }
            runtime.active.remove(&id);
        }
        control.done.store(true, Ordering::Release);
        control.finished.notify_waiters();
    }

    fn settle_transfer(
        &self,
        id: &str,
        command: u8,
        result: Result<(), ArtifactError>,
    ) -> Result<(), ArtifactError> {
        if matches!(&result, Err(ArtifactError::RootUnavailable)) {
            return Err(ArtifactError::RootUnavailable);
        }
        let state = self
            .runtime
            .lock()
            .ok()
            .and_then(|runtime| runtime.manifest.jobs.get(id).map(|job| job.state))
            .ok_or(ArtifactError::Corrupt)?;
        if state == JobState::Complete {
            Ok(())
        } else if state == JobState::Publishing {
            self.set_state(
                id,
                JobState::Publishing,
                Some("artifact_publication_uncertain".to_owned()),
            )
        } else if command == CANCEL {
            self.cancel_finished(id)
        } else if command == PAUSE {
            self.set_state(id, JobState::Paused, None)
        } else {
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let state = if matches!(
                        error,
                        ArtifactError::HashMismatch
                            | ArtifactError::SizeMismatch
                            | ArtifactError::Corrupt
                            | ArtifactError::SourceChanged
                    ) {
                        JobState::Quarantined
                    } else {
                        JobState::Failed
                    };
                    self.set_state(id, state, Some(error.code().to_owned()))
                }
            }
        }
    }

    async fn transfer(
        self: &Arc<Self>,
        id: &str,
        control: &Arc<Control>,
        import: Option<File>,
    ) -> Result<(), ArtifactError> {
        let store = self.clone();
        let started_id = id.to_owned();
        let preparation_control = control.clone();
        let (file, mut progress, job) = tokio::task::spawn_blocking(move || {
            store.prepare_target(&started_id, &preparation_control)
        })
        .await
        .map_err(|_| ArtifactError::Io(io::Error::other("Artifact preparation task failed")))??;
        let mut validator = job.validator.clone();
        let transfer = if let Some(source) = import {
            self.copy_import(id, control, source, &file, &mut progress, &job)
                .await
        } else {
            self.copy_download(id, control, &file, &mut progress, &mut validator, &job)
                .await
        };
        if matches!(&transfer, Err(ArtifactError::RootUnavailable))
            || control.storage_refused.load(Ordering::Acquire)
        {
            return Err(ArtifactError::RootUnavailable);
        }
        let finish_store = self.clone();
        let finish_file = file.clone();
        tokio::task::spawn_blocking(move || {
            finish_store.verify_mutation()?;
            let mut writer = &*finish_file;
            writer.flush()?;
            finish_store.verify_mutation()?;
            finish_file.sync_all()?;
            Ok::<_, ArtifactError>(())
        })
        .await
        .map_err(|_| ArtifactError::Io(io::Error::other("Artifact sync task failed")))??;
        self.checkpoint_async(id, progress.bytes, validator).await?;
        transfer?;
        if control.command.load(Ordering::Acquire) != RUN {
            return Err(ArtifactError::Stop);
        }
        if progress.bytes == 0
            || progress.bytes > job.maximum_bytes
            || job
                .expected_bytes
                .is_some_and(|bytes| bytes != progress.bytes)
        {
            return Err(ArtifactError::SizeMismatch);
        }
        let actual = format!("{:x}", progress.digest.finalize());
        if !job.computed_digest && actual != job.expected_sha256 {
            return Err(ArtifactError::HashMismatch);
        }
        let computed = job.computed_digest;
        let store = self.clone();
        let id = id.to_owned();
        let control = control.clone();
        tokio::task::spawn_blocking(move || {
            if computed {
                store.learn_digest(&id, &actual)?;
            }
            store.publish(&id, progress.bytes, &actual, &file, &control)
        })
        .await
        .map_err(|_| ArtifactError::Io(io::Error::other("Artifact publication task failed")))?
    }

    fn prepare_target(
        &self,
        id: &str,
        control: &Control,
    ) -> Result<(Arc<File>, TransferProgress, Job), ArtifactError> {
        if control.command.load(Ordering::Acquire) != RUN {
            return Err(ArtifactError::Stop);
        }
        self.set_state(id, JobState::Downloading, None)?;
        let job = self
            .runtime
            .lock()
            .map_err(|_| ArtifactError::Corrupt)?
            .manifest
            .jobs
            .get(id)
            .cloned()
            .ok_or(ArtifactError::NotFound)?;
        self.verify_mutation()?;
        let path = self.staging_path(id);
        let partial = match regular(&path) {
            Ok(file) => file,
            Err(ArtifactError::Io(error))
                if error.kind() == io::ErrorKind::NotFound
                    && (job.restart || job.downloaded_bytes == 0) =>
            {
                self.create_staging(id)?
            }
            Err(error) => return Err(error),
        };
        self.verify_mutation()?;
        let mut file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
        if !same_file(&partial, &path)? || !same_file(&file, &path)? {
            return Err(ArtifactError::Corrupt);
        }
        let resume = job.source_url.is_some() && !job.restart && job.validator.is_some();
        if !resume {
            self.verify_mutation()?;
            file.set_len(0)?;
        }
        file.seek(SeekFrom::Start(0))?;
        let maximum = job
            .expected_bytes
            .unwrap_or(job.maximum_bytes)
            .min(job.maximum_bytes);
        let mut progress = TransferProgress {
            digest: Sha256::new(),
            bytes: 0,
        };
        let mut buffer = [0u8; 8192];
        loop {
            if control.command.load(Ordering::Acquire) != RUN {
                return Err(ArtifactError::Stop);
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            progress.bytes = progress
                .bytes
                .checked_add(count as u64)
                .ok_or(ArtifactError::SizeMismatch)?;
            if progress.bytes > maximum {
                return Err(ArtifactError::SizeMismatch);
            }
            progress.digest.update(&buffer[..count]);
        }
        Ok((Arc::new(file), progress, job))
    }

    async fn copy_import(
        self: &Arc<Self>,
        id: &str,
        control: &Arc<Control>,
        mut source: File,
        target: &Arc<File>,
        progress: &mut TransferProgress,
        job: &Job,
    ) -> Result<(), ArtifactError> {
        let store = self.clone();
        let id = id.to_owned();
        let control = control.clone();
        let target = target.clone();
        let mut pending = TransferProgress {
            digest: progress.digest.clone(),
            bytes: progress.bytes,
        };
        let maximum = job.maximum_bytes;
        let expected = job.expected_bytes;
        let (result, updated) = tokio::task::spawn_blocking(move || {
            let result = (|| {
                store.verify_mutation()?;
                source.seek(SeekFrom::Start(0))?;
                let mut buffer = [0u8; 65536];
                let mut checkpoint = 0;
                let mut checkpoint_time = Instant::now();
                loop {
                    if control.command.load(Ordering::Acquire) != RUN {
                        return Err(ArtifactError::Stop);
                    }
                    let count = source.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    let next = pending
                        .bytes
                        .checked_add(count as u64)
                        .ok_or(ArtifactError::SizeMismatch)?;
                    if next > maximum || expected.is_some_and(|bytes| next > bytes) {
                        return Err(ArtifactError::SizeMismatch);
                    }
                    store.verify_mutation()?;
                    if control.command.load(Ordering::Acquire) != RUN {
                        return Err(ArtifactError::Stop);
                    }
                    let mut writer = &*target;
                    writer.write_all(&buffer[..count])?;
                    pending.digest.update(&buffer[..count]);
                    pending.bytes = next;
                    if checkpoint == 0
                        || pending.bytes - checkpoint >= 16 * 1024 * 1024
                        || checkpoint_time.elapsed() >= Duration::from_secs(1)
                    {
                        store.checkpoint(&id, pending.bytes, None)?;
                        checkpoint = pending.bytes;
                        checkpoint_time = Instant::now();
                    }
                }
                Ok(())
            })();
            (result, pending)
        })
        .await
        .map_err(|_| ArtifactError::Io(io::Error::other("Artifact import task failed")))?;
        *progress = updated;
        result
    }

    async fn write_chunk(
        self: &Arc<Self>,
        target: &Arc<File>,
        control: &Arc<Control>,
        chunk: Bytes,
    ) -> Result<(), ArtifactError> {
        let store = self.clone();
        let target = target.clone();
        let control = control.clone();
        tokio::task::spawn_blocking(move || {
            store.verify_mutation()?;
            if control.command.load(Ordering::Acquire) != RUN {
                return Err(ArtifactError::Stop);
            }
            let mut writer = &*target;
            writer.write_all(&chunk)?;
            Ok(())
        })
        .await
        .map_err(|_| ArtifactError::Io(io::Error::other("Artifact write task failed")))?
    }

    async fn reset_target(
        self: &Arc<Self>,
        target: &Arc<File>,
        control: &Arc<Control>,
    ) -> Result<(), ArtifactError> {
        let store = self.clone();
        let target = target.clone();
        let control = control.clone();
        tokio::task::spawn_blocking(move || {
            store.verify_mutation()?;
            if control.command.load(Ordering::Acquire) != RUN {
                return Err(ArtifactError::Stop);
            }
            target.set_len(0)?;
            let mut writer = &*target;
            writer.seek(SeekFrom::Start(0))?;
            Ok(())
        })
        .await
        .map_err(|_| ArtifactError::Io(io::Error::other("Artifact reset task failed")))?
    }

    async fn copy_download(
        self: &Arc<Self>,
        id: &str,
        control: &Arc<Control>,
        target: &Arc<File>,
        progress: &mut TransferProgress,
        validator: &mut Option<String>,
        job: &Job,
    ) -> Result<(), ArtifactError> {
        let TransferProgress {
            digest,
            bytes: offset,
        } = progress;
        let source = job.source_url.as_deref().ok_or(ArtifactError::Invalid)?;
        if job.expected_bytes == Some(*offset)
            && format!("{:x}", digest.clone().finalize()) == job.expected_sha256
        {
            return Ok(());
        }
        let mut url = source_url(source, self.policy, false)?;
        let mut response = None;
        for _ in 0..=self.limits.maximum_redirects {
            if control.command.load(Ordering::Acquire) != RUN {
                return Err(ArtifactError::Stop);
            }
            let client = tokio::select! {
                biased;
                _ = control.changed.notified() => { return Err(ArtifactError::Stop); }
                client = self.client(&url) => client?,
            };
            let scope = self.scope.clone();
            tokio::task::spawn_blocking(move || scope.verify())
                .await
                .map_err(|_| {
                    ArtifactError::Io(io::Error::other("Artifact root inspection task failed"))
                })?
                .map_err(|_| ArtifactError::RootUnavailable)?;
            let mut request = client
                .get(url.clone())
                .header(header::ACCEPT_ENCODING, "identity")
                .timeout(self.limits.request_timeout);
            if *offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
                if let Some(value) = validator.as_deref() {
                    request = request.header(header::IF_RANGE, value);
                }
            }
            let mut sent = Box::pin(request.send());
            let received = loop {
                tokio::select! {
                    biased;
                    _ = control.changed.notified() => { if control.command.load(Ordering::Acquire) != RUN { return Err(ArtifactError::Stop); } }
                    value = &mut sent => break value.map_err(|_| ArtifactError::Network)?,
                }
            };
            if received.status().is_redirection() {
                let location = received
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(ArtifactError::SourcePolicy)?;
                let next = url
                    .join(location)
                    .map_err(|_| ArtifactError::SourcePolicy)?;
                url = source_url(next.as_str(), self.policy, true)?;
                continue;
            }
            response = Some(received);
            break;
        }
        let mut response = response.ok_or(ArtifactError::SourcePolicy)?;
        let received_validator = strong_validator(response.headers());
        if *offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT {
            if received_validator.as_ref() != validator.as_ref() {
                return Err(ArtifactError::SourceChanged);
            }
            let range = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .ok_or(ArtifactError::SourceChanged)?;
            let total = validate_range(range, *offset)?;
            if total > job.maximum_bytes
                || job.expected_bytes.is_some_and(|expected| expected != total)
            {
                return Err(ArtifactError::SizeMismatch);
            }
            if response
                .content_length()
                .is_some_and(|length| length != total - *offset)
            {
                return Err(ArtifactError::SourceChanged);
            }
        } else if response.status() == StatusCode::OK {
            if *offset > 0 {
                self.reset_target(target, control).await?;
                *offset = 0;
                *digest = Sha256::new();
            }
        } else {
            return Err(ArtifactError::Network);
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| value != "identity")
        {
            return Err(ArtifactError::SourcePolicy);
        }
        if response.content_length().is_some_and(|length| {
            length > job.maximum_bytes.saturating_sub(*offset)
                || job
                    .expected_bytes
                    .is_some_and(|expected| length != expected.saturating_sub(*offset))
        }) {
            return Err(ArtifactError::SizeMismatch);
        }
        *validator = received_validator;
        self.checkpoint_async(id, *offset, validator.clone())
            .await?;
        let mut checkpoint = *offset;
        let mut checkpoint_time = Instant::now();
        loop {
            if control.command.load(Ordering::Acquire) != RUN {
                return Err(ArtifactError::Stop);
            }
            let chunk = loop {
                tokio::select! {
                    biased;
                    _ = control.changed.notified() => { if control.command.load(Ordering::Acquire) != RUN { return Err(ArtifactError::Stop); } }
                    value = response.chunk() => break value.map_err(|_| ArtifactError::Network)?,
                }
            };
            let Some(chunk) = chunk else {
                break;
            };
            let next = offset
                .checked_add(chunk.len() as u64)
                .ok_or(ArtifactError::SizeMismatch)?;
            if next > job.maximum_bytes
                || job.expected_bytes.is_some_and(|expected| next > expected)
            {
                return Err(ArtifactError::SizeMismatch);
            }
            self.write_chunk(target, control, chunk.clone()).await?;
            digest.update(&chunk);
            *offset = next;
            if checkpoint == 0
                || *offset - checkpoint >= 16 * 1024 * 1024
                || checkpoint_time.elapsed() >= Duration::from_secs(1)
            {
                self.checkpoint_async(id, *offset, validator.clone())
                    .await?;
                checkpoint = *offset;
                checkpoint_time = Instant::now();
            }
        }
        Ok(())
    }

    async fn client(&self, url: &Url) -> Result<reqwest::Client, ArtifactError> {
        policy_client(url, self.policy).await
    }

    async fn checkpoint_async(
        self: &Arc<Self>,
        id: &str,
        bytes: u64,
        validator: Option<String>,
    ) -> Result<(), ArtifactError> {
        let store = self.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || store.checkpoint(&id, bytes, validator))
            .await
            .map_err(|_| ArtifactError::Io(io::Error::other("Artifact checkpoint task failed")))?
    }

    fn checkpoint(
        &self,
        id: &str,
        bytes: u64,
        validator: Option<String>,
    ) -> Result<(), ArtifactError> {
        self.verify_mutation()?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut next = runtime.manifest.clone();
        let job = next.jobs.get_mut(id).ok_or(ArtifactError::NotFound)?;
        job.downloaded_bytes = bytes;
        job.validator = validator;
        self.commit(&mut runtime, next)
    }

    fn set_state(
        &self,
        id: &str,
        state: JobState,
        error: Option<String>,
    ) -> Result<(), ArtifactError> {
        self.verify_mutation()?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut next = runtime.manifest.clone();
        let job = next.jobs.get_mut(id).ok_or(ArtifactError::NotFound)?;
        if matches!(job.state, JobState::Pausing | JobState::Cancelling)
            && state == JobState::Downloading
        {
            return Ok(());
        }
        job.state = state;
        job.error = error;
        job.revision = job.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
        self.commit(&mut runtime, next)?;
        runtime.failures.remove(id);
        Ok(())
    }

    fn cancel_finished(&self, id: &str) -> Result<(), ArtifactError> {
        self.verify_mutation()?;
        let path = self.staging_path(id);
        match regular(&path) {
            Ok(_) => {
                self.verify_mutation()?;
                fs::remove_file(path)?;
            }
            Err(ArtifactError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.set_state(id, JobState::Cancelled, None)
    }

    fn publish(
        &self,
        id: &str,
        bytes: u64,
        sha256: &str,
        validated: &File,
        control: &Control,
    ) -> Result<(), ArtifactError> {
        self.publish_checked(id, bytes, sha256, validated, control, || {})
    }

    fn publish_checked(
        &self,
        id: &str,
        bytes: u64,
        sha256: &str,
        validated: &File,
        control: &Control,
        before_verify: impl FnOnce(),
    ) -> Result<(), ArtifactError> {
        let _publication = self
            .publication
            .lock()
            .map_err(|_| ArtifactError::Corrupt)?;
        self.verify_mutation()?;
        let held = {
            let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
            if control.command.load(Ordering::Acquire) != RUN {
                return Err(ArtifactError::Stop);
            }
            let job = runtime
                .manifest
                .jobs
                .get(id)
                .ok_or(ArtifactError::NotFound)?;
            if job.expected_sha256 != sha256 {
                return Err(ArtifactError::HashMismatch);
            }
            runtime
                .manifest
                .artifacts
                .get_mut(sha256)
                .map(|existing| {
                    if existing.bytes != bytes || existing.removing {
                        return Err(ArtifactError::Corrupt);
                    }
                    Hold::acquire(&mut existing.holds)
                })
                .transpose()?
        };
        let source = self.staging_path(id);
        let target = self.artifact_path(sha256);
        before_verify();
        self.verify_mutation()?;
        let verified_target = if held.is_some() {
            let (file, metadata, length, digest) =
                hash_checked(&target, self.limits.maximum_artifact_bytes, || {
                    if control.command.load(Ordering::Acquire) == RUN {
                        Ok(())
                    } else {
                        Err(ArtifactError::Stop)
                    }
                })?;
            if length != bytes || digest != sha256 {
                return Err(ArtifactError::Corrupt);
            }
            Some((file, metadata))
        } else {
            None
        };
        self.verify_mutation()?;
        let source_file = regular(&source)?;
        if !same_file(validated, &source)? || source_file.metadata()?.len() != bytes {
            return Err(ArtifactError::Corrupt);
        }
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if control.command.load(Ordering::Acquire) != RUN {
            return Err(ArtifactError::Stop);
        }
        let job = runtime
            .manifest
            .jobs
            .get(id)
            .ok_or(ArtifactError::NotFound)?;
        if job.expected_sha256 != sha256 {
            return Err(ArtifactError::HashMismatch);
        }
        let mut next = runtime.manifest.clone();
        let record = next.jobs.get_mut(id).expect("existing job");
        record.state = JobState::Publishing;
        record.revision = record.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
        self.commit(&mut runtime, next)?;
        drop(runtime);
        if let Some((file, before)) = verified_target {
            let after = file.metadata()?;
            if !same_file(&file, &target)?
                || after.len() != bytes
                || before.modified()? != after.modified()?
            {
                return Err(ArtifactError::Corrupt);
            }
            self.verify_mutation()?;
            if !same_file(validated, &source)? {
                return Err(ArtifactError::Corrupt);
            }
            fs::remove_file(source)?;
        } else {
            if target.exists() {
                return Err(ArtifactError::Corrupt);
            }
            self.verify_mutation()?;
            fs::hard_link(&source, &target)?;
            if !same_file(validated, &target)? || !same_file(validated, &source)? {
                return Err(ArtifactError::Corrupt);
            }
            self.verify_mutation()?;
            fs::remove_file(&source)?;
        }
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let mut next = runtime.manifest.clone();
        next.artifacts
            .entry(sha256.to_owned())
            .or_insert_with(|| Artifact {
                bytes,
                sha256: sha256.to_owned(),
                references: BTreeSet::new(),
                removing: false,
                holds: None,
            });
        let job = next.jobs.get_mut(id).expect("publishing job");
        job.state = JobState::Complete;
        job.artifact = Some(sha256.to_owned());
        job.error = None;
        job.revision = job.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
        self.commit(&mut runtime, next)
    }

    fn commit(&self, runtime: &mut Runtime, mut next: Manifest) -> Result<(), ArtifactError> {
        next.generation = runtime
            .manifest
            .generation
            .checked_add(1)
            .ok_or(ArtifactError::Limit)?;
        manifest_within_limit(&next, self.limits.maximum_manifest_bytes)?;
        let result = self.scope.write_json(&self.lease, MANIFEST, &next);
        if result
            .as_ref()
            .map_or_else(AtomicWriteError::committed, |_| true)
        {
            runtime.replace_manifest(next);
        }
        result.map(|_| ()).map_err(Into::into)
    }

    fn verify_mutation(&self) -> Result<(), ArtifactError> {
        self.scope
            .verify()
            .map_err(|_| ArtifactError::RootUnavailable)
    }

    fn create_staging(&self, id: &str) -> Result<File, ArtifactError> {
        self.verify_mutation()?;
        Ok(fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.staging_path(id))?)
    }

    fn staging_path(&self, id: &str) -> PathBuf {
        self.lease.root().join(format!("job-{id}.part"))
    }
    fn artifact_path(&self, id: &str) -> PathBuf {
        self.lease.root().join(format!("artifact-{id}.bin"))
    }
}

fn effective_state(id: &str, job: &Job, runtime: &Runtime) -> JobState {
    if !runtime.active.contains_key(id)
        && matches!(
            job.state,
            JobState::Queued
                | JobState::Downloading
                | JobState::Pausing
                | JobState::Cancelling
                | JobState::Publishing
        )
    {
        JobState::Interrupted
    } else {
        job.state
    }
}

fn job_view(id: &str, job: &Job, runtime: &Runtime) -> JobView {
    let source_url_bytes = job.source_url.as_ref().map_or(0, String::len);
    JobView {
        id: id.to_owned(),
        revision: job.revision,
        state: effective_state(id, job, runtime),
        source_url: job
            .source_url
            .as_deref()
            .map(|url| inventory::truncate(url, inventory::SOURCE_URL_PREVIEW_BYTES).to_owned()),
        source_url_bytes,
        source_url_truncated: source_url_bytes > inventory::SOURCE_URL_PREVIEW_BYTES,
        expected_sha256: job.expected_sha256.clone(),
        expected_bytes: job.expected_bytes,
        maximum_bytes: job.maximum_bytes,
        downloaded_bytes: job.downloaded_bytes,
        artifact: job.artifact.clone(),
        error: runtime
            .failures
            .get(id)
            .map(|code| (*code).to_owned())
            .or_else(|| {
                job.error.as_deref().map(|error| {
                    inventory::truncate(error, inventory::ERROR_PREVIEW_BYTES).to_owned()
                })
            })
            .or_else(|| {
                (job.state == JobState::Publishing && !runtime.active.contains_key(id))
                    .then(|| "artifact_publication_uncertain".to_owned())
            }),
        writer_active: runtime.active.contains_key(id),
        source_name: job.source_name.clone(),
        computed_digest: job.computed_digest,
    }
}

fn artifact_view(id: &str, artifact: &Artifact) -> ArtifactView {
    ArtifactView {
        id: id.to_owned(),
        bytes: artifact.bytes,
        sha256: artifact.sha256.clone(),
        references: artifact.references.iter().cloned().collect(),
        active_holds: artifact.active_holds(),
        removing: artifact.removing,
    }
}

fn charged(runtime: &Runtime) -> u64 {
    runtime
        .manifest
        .artifacts
        .values()
        .map(|artifact| artifact.bytes)
        .chain(
            runtime
                .manifest
                .jobs
                .values()
                .filter(|job| !matches!(job.state, JobState::Complete | JobState::Cancelled))
                .map(|job| job.maximum_bytes.max(job.downloaded_bytes)),
        )
        .chain(runtime.quarantined_files.values().copied())
        .fold(0, u64::saturating_add)
}

fn orphan_files(
    lease: &DirectoryLease,
    manifest: &Manifest,
    limits: &ArtifactLimits,
) -> Result<BTreeMap<String, u64>, ArtifactError> {
    lease.verify()?;
    let maximum_entries = limits
        .maximum_jobs
        .checked_add(limits.maximum_artifacts)
        .and_then(|count| count.checked_add(64))
        .ok_or(ArtifactError::Limit)?;
    let mut quarantined = BTreeMap::new();
    for (index, entry) in fs::read_dir(lease.root())?.enumerate() {
        if index >= maximum_entries {
            return Err(ArtifactError::Limit);
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let staging = name
            .strip_prefix("job-")
            .and_then(|name| name.strip_suffix(".part"))
            .filter(|id| job_id(id).is_ok());
        let artifact = name
            .strip_prefix("artifact-")
            .and_then(|name| name.strip_suffix(".bin"))
            .filter(|id| artifact_id(id).is_ok());
        if staging.is_none() && artifact.is_none() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::Corrupt);
        }
        if let Some(job) = staging.and_then(|id| manifest.jobs.get(id))
            && !matches!(job.state, JobState::Complete | JobState::Cancelled)
        {
            if metadata.len() > job.maximum_bytes {
                return Err(ArtifactError::Corrupt);
            }
            continue;
        }
        if let Some(artifact) = artifact.and_then(|id| manifest.artifacts.get(id)) {
            if metadata.len() != artifact.bytes {
                return Err(ArtifactError::Corrupt);
            }
            continue;
        }
        if let Some(id) = artifact
            && manifest.jobs.values().any(|job| {
                job.state == JobState::Publishing
                    && job.expected_sha256 == id
                    && metadata.len() <= job.maximum_bytes
            })
        {
            continue;
        }
        quarantined.insert(
            name.to_owned(),
            limits.maximum_artifact_bytes.max(metadata.len()),
        );
    }
    lease.verify()?;
    Ok(quarantined)
}

fn manifest_within_limit(manifest: &Manifest, maximum: u64) -> Result<(), ArtifactError> {
    struct Count(u64);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len() as u64)
                .ok_or_else(|| io::Error::other("Manifest size limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(
        Count(maximum.checked_sub(1).ok_or(ArtifactError::Limit)?),
        manifest,
    )
    .map_err(|_| ArtifactError::Limit)
}

fn validate_expected(
    digest: &str,
    expected: Option<u64>,
    maximum: u64,
    limits: &ArtifactLimits,
) -> Result<(), ArtifactError> {
    if !digest_valid(digest)
        || maximum == 0
        || maximum > limits.maximum_artifact_bytes
        || expected.is_some_and(|bytes| bytes == 0 || bytes > maximum)
    {
        return Err(ArtifactError::Invalid);
    }
    Ok(())
}

fn job_id(id: &str) -> Result<(), ArtifactError> {
    if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(ArtifactError::Invalid)
    }
}
fn artifact_id(id: &str) -> Result<(), ArtifactError> {
    if digest_valid(id) && id == id.to_ascii_lowercase() {
        Ok(())
    } else {
        Err(ArtifactError::Invalid)
    }
}
fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn reference_valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

pub(crate) async fn policy_client(
    url: &Url,
    policy: SourcePolicy,
) -> Result<reqwest::Client, ArtifactError> {
    let host = url.host_str().ok_or(ArtifactError::SourcePolicy)?;
    let port = url
        .port_or_known_default()
        .ok_or(ArtifactError::SourcePolicy)?;
    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((host.trim_matches(['[', ']']), port)),
    )
    .await
    .map_err(|_| ArtifactError::Network)?
    .map_err(|_| ArtifactError::Network)?
    .collect();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !allowed_address(address.ip(), url.scheme(), policy))
    {
        return Err(ArtifactError::SourcePolicy);
    }
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| ArtifactError::Network)
}

pub(crate) fn policy_url(value: &str, policy: SourcePolicy) -> Result<Url, ArtifactError> {
    source_url(value, policy, true)
}

pub(crate) fn source_url(
    value: &str,
    policy: SourcePolicy,
    redirect: bool,
) -> Result<Url, ArtifactError> {
    let url = Url::parse(value).map_err(|_| ArtifactError::SourcePolicy)?;
    if policy == SourcePolicy::Offline {
        return Err(ArtifactError::Offline);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (!redirect && url.query().is_some())
    {
        return Err(ArtifactError::SourcePolicy);
    }
    if url.scheme() != "https"
        && !(url.scheme() == "http" && policy == SourcePolicy::AllowLoopbackHttp)
    {
        return Err(ArtifactError::SourcePolicy);
    }
    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>()
            && !allowed_address(ip, url.scheme(), policy)
        {
            return Err(ArtifactError::SourcePolicy);
        }
    } else {
        return Err(ArtifactError::SourcePolicy);
    }
    Ok(url)
}

fn allowed_address(ip: IpAddr, scheme: &str, policy: SourcePolicy) -> bool {
    if scheme == "http" {
        return policy == SourcePolicy::AllowLoopbackHttp && ip.is_loopback();
    }
    match ip {
        IpAddr::V4(ip) => {
            let bytes = ip.octets();
            !ip.is_loopback()
                && !ip.is_private()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_multicast()
                && !ip.is_unspecified()
                && !ip.is_documentation()
                && bytes[0] != 0
                && bytes[0] < 224
                && !(bytes[0] == 192 && bytes[1] == 0 && bytes[2] == 0)
                && !(bytes[0] == 192 && bytes[1] == 88 && bytes[2] == 99)
                && !(bytes[0] == 100 && (64..=127).contains(&bytes[1]))
                && !(bytes[0] == 198 && (bytes[1] == 18 || bytes[1] == 19))
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return allowed_address(IpAddr::V4(ip), scheme, policy);
            }
            let segments = ip.segments();
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] < 0x0200)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
        }
    }
}

fn strong_validator(headers: &header::HeaderMap) -> Option<String> {
    headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() <= 256
                && value.starts_with('"')
                && value.ends_with('"')
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn validate_range(value: &str, offset: u64) -> Result<u64, ArtifactError> {
    let range = value
        .strip_prefix("bytes ")
        .ok_or(ArtifactError::SourceChanged)?;
    let (span, total) = range.split_once('/').ok_or(ArtifactError::SourceChanged)?;
    let (start, end) = span.split_once('-').ok_or(ArtifactError::SourceChanged)?;
    let start: u64 = start.parse().map_err(|_| ArtifactError::SourceChanged)?;
    let end: u64 = end.parse().map_err(|_| ArtifactError::SourceChanged)?;
    let total: u64 = total.parse().map_err(|_| ArtifactError::SourceChanged)?;
    if start != offset || end < start || end.checked_add(1) != Some(total) {
        return Err(ArtifactError::SourceChanged);
    }
    Ok(total)
}

fn regular(path: &Path) -> Result<File, ArtifactError> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(ArtifactError::Corrupt);
    }
    let file = File::open(path)?;
    if !same_file(&file, path)? || fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(ArtifactError::Corrupt);
    }
    Ok(file)
}

fn hash(path: &Path, maximum: u64) -> Result<(u64, String), ArtifactError> {
    let (_, _, bytes, digest) = hash_checked(path, maximum, || Ok(()))?;
    Ok((bytes, digest))
}

fn hash_checked(
    path: &Path,
    maximum: u64,
    mut check: impl FnMut() -> Result<(), ArtifactError>,
) -> Result<(File, fs::Metadata, u64, String), ArtifactError> {
    check()?;
    let mut file = regular(path)?;
    let before = file.metadata()?;
    if before.len() > maximum {
        return Err(ArtifactError::Limit);
    }
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > maximum {
            return Err(ArtifactError::Limit);
        }
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if bytes != before.len() || before.modified()? != after.modified()? || !same_file(&file, path)?
    {
        return Err(ArtifactError::Corrupt);
    }
    check()?;
    Ok((file, before, bytes, format!("{:x}", digest.finalize())))
}

#[cfg(test)]
mod inventory_tests;
#[cfg(test)]
mod publication_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, Response},
        response::IntoResponse,
        routing::get,
    };
    use futures_util::stream;

    struct Source {
        bytes: Arc<Vec<u8>>,
        ranges: Mutex<Vec<(Option<String>, Option<String>)>>,
        mode: AtomicU8,
        waiting: AtomicBool,
        released: AtomicBool,
        release: Notify,
    }

    struct HttpFixture {
        source: Arc<Source>,
        base: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn file(
        State(source): State<Arc<Source>>,
        headers: HeaderMap,
    ) -> axum::response::Response {
        let range = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let validator = headers
            .get(header::IF_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        source
            .ranges
            .lock()
            .unwrap()
            .push((range.clone(), validator));
        let mode = source.mode.load(Ordering::Acquire);
        if mode == 4 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let start = if mode == 2 {
            0
        } else {
            range
                .as_deref()
                .and_then(|value| value.strip_prefix("bytes="))
                .and_then(|value| value.strip_suffix('-'))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
        };
        if start >= source.bytes.len() {
            return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        }
        let body_source = source.clone();
        let body = stream::unfold(start, move |position| {
            let source = body_source.clone();
            async move {
                if position == source.bytes.len() {
                    return None;
                }
                if source.mode.load(Ordering::Acquire) == 5 && position > 0 {
                    let release = source.release.notified();
                    tokio::pin!(release);
                    release.as_mut().enable();
                    source.waiting.store(true, Ordering::Release);
                    if !source.released.load(Ordering::Acquire) {
                        release.await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
                let end = (position + 4096).min(source.bytes.len());
                Some((
                    Ok::<_, io::Error>(source.bytes[position..end].to_vec()),
                    end,
                ))
            }
        });
        let mut response = Response::builder()
            .status(if start > 0 {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            })
            .header(
                header::ETAG,
                if mode == 1 && start > 0 {
                    "\"fixture-v2\""
                } else {
                    "\"fixture-v1\""
                },
            )
            .header(
                header::CONTENT_LENGTH,
                (source.bytes.len() - start).to_string(),
            );
        if start > 0 {
            response = response.header(
                header::CONTENT_RANGE,
                format!(
                    "bytes {}-{}/{}",
                    if mode == 3 { start + 1 } else { start },
                    source.bytes.len() - 1,
                    source.bytes.len()
                ),
            );
        }
        response.body(Body::from_stream(body)).unwrap()
    }

    async fn fixture() -> HttpFixture {
        let source = Arc::new(Source {
            bytes: Arc::new((0..256 * 1024).map(|index| (index % 251) as u8).collect()),
            ranges: Mutex::new(Vec::new()),
            mode: AtomicU8::new(0),
            waiting: AtomicBool::new(false),
            released: AtomicBool::new(false),
            release: Notify::new(),
        });
        let app = Router::new()
            .route("/file", get(file))
            .route(
                "/redirect",
                get(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(
                            header::LOCATION,
                            "/file?X-Amz-Signature=ephemeral-test-only",
                        )],
                    )
                }),
            )
            .with_state(source.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        HttpFixture { source, base, task }
    }

    pub(super) fn store() -> (tempfile::TempDir, Arc<ArtifactStore>) {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(
            DirectoryLease::acquire(directory.path().join("artifacts")).unwrap(),
            ArtifactLimits::default(),
            SourcePolicy::AllowLoopbackHttp,
        )
        .unwrap();
        (directory, store)
    }

    #[cfg(unix)]
    fn scoped_store() -> (tempfile::TempDir, Arc<ArtifactStore>) {
        let directory = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(directory.path().join("artifacts")).unwrap();
        let neighbor = DirectoryLease::acquire(directory.path().join("neighbor")).unwrap();
        let scope = LiveRoots::new(vec![lease.clone(), neighbor]).unwrap();
        let store = ArtifactStore::open_scoped(
            lease,
            ArtifactLimits::default(),
            SourcePolicy::AllowLoopbackHttp,
            scope,
        )
        .unwrap();
        (directory, store)
    }

    #[cfg(unix)]
    fn replace_neighbor(directory: &Path) -> PathBuf {
        let neighbor = directory.join("neighbor");
        fs::rename(&neighbor, directory.join("retained-neighbor")).unwrap();
        fs::create_dir(&neighbor).unwrap();
        fs::write(
            neighbor.join("foreign-canary"),
            b"foreign bytes stay unchanged",
        )
        .unwrap();
        neighbor
    }

    #[cfg(unix)]
    fn restore_neighbor(directory: &Path) {
        fs::rename(
            directory.join("neighbor"),
            directory.join("foreign-neighbor"),
        )
        .unwrap();
        fs::rename(
            directory.join("retained-neighbor"),
            directory.join("neighbor"),
        )
        .unwrap();
    }

    #[cfg(unix)]
    async fn until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("artifact observation reaches its test bound");
    }

    #[cfg(unix)]
    fn reserved_job() -> Job {
        Job {
            revision: 1,
            state: JobState::Queued,
            source_url: Some("http://127.0.0.1:1/file".into()),
            expected_sha256: format!("{:x}", Sha256::digest(b"safe")),
            expected_bytes: Some(4),
            maximum_bytes: 4,
            downloaded_bytes: 0,
            validator: None,
            artifact: None,
            error: None,
            restart: false,
            source_name: None,
            computed_digest: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn durable_generated_intent_precedes_staging_creation_and_survives_that_interruption() {
        let (_directory, store) = scoped_store();
        let id = {
            let mut runtime = store.runtime.lock().unwrap();
            store.reserve_intent(&mut runtime, reserved_job()).unwrap()
        };
        let durable: Manifest =
            serde_json::from_slice(&fs::read(store.lease.root().join(MANIFEST)).unwrap()).unwrap();
        assert!(durable.jobs.contains_key(&id));
        assert!(!store.staging_path(&id).exists());
        assert_eq!(store.inspect().unwrap().charged_bytes, 4);
        let lease = store.lease.clone();
        let scope = store.scope.clone();
        drop(store);
        let restarted = ArtifactStore::open_scoped(
            lease,
            ArtifactLimits::default(),
            SourcePolicy::AllowLoopbackHttp,
            scope,
        )
        .unwrap();
        let interrupted = restarted.job(&id).unwrap();
        assert_eq!(interrupted.state, JobState::Interrupted);
        assert!(!interrupted.writer_active);
        assert_eq!(restarted.inspect().unwrap().charged_bytes, 4);
        assert_eq!(
            restarted.cancel(&id, interrupted.revision).unwrap().state,
            JobState::Cancelled
        );
        assert_eq!(restarted.inspect().unwrap().charged_bytes, 0);
    }

    #[cfg(unix)]
    #[test]
    fn root_loss_after_intent_blocks_staging_creation_without_unlinking_or_releasing_charge() {
        let (directory, store) = scoped_store();
        let id = {
            let mut runtime = store.runtime.lock().unwrap();
            store.reserve_intent(&mut runtime, reserved_job()).unwrap()
        };
        let before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let neighbor = replace_neighbor(directory.path());
        assert_eq!(
            store.create_staging(&id).unwrap_err().code(),
            "storage_root_unavailable"
        );
        assert!(!store.staging_path(&id).exists());
        assert_eq!(store.inspect().unwrap().charged_bytes, 4);
        let view = store.job(&id).unwrap();
        assert_eq!(
            store.cancel(&id, view.revision).unwrap_err().code(),
            "storage_root_unavailable"
        );
        assert_eq!(fs::read(store.lease.root().join(MANIFEST)).unwrap(), before);
        assert_eq!(
            fs::read(neighbor.join("foreign-canary")).unwrap(),
            b"foreign bytes stay unchanged"
        );
    }

    #[test]
    fn failed_initial_intent_publication_retains_quarantine_without_creating_unjournaled_staging() {
        let (_directory, store) = store();
        let manifest = store.lease.root().join(MANIFEST);
        fs::rename(&manifest, store.lease.root().join("retained-manifest")).unwrap();
        fs::create_dir(&manifest).unwrap();
        let error = store
            .begin(None, "a".repeat(64), Some(4), 4, 0)
            .err()
            .unwrap();
        assert_eq!(error.code(), "artifact_persistence");
        assert_eq!(
            crate::ApiError::from(error).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let observed = store.inspect().unwrap();
        assert_eq!(observed.jobs.len(), 1);
        assert_eq!(observed.jobs[0].state, JobState::Quarantined);
        assert_eq!(
            observed.jobs[0].error.as_deref(),
            Some("artifact_intent_uncertain")
        );
        assert!(!observed.jobs[0].writer_active);
        assert_eq!(observed.charged_bytes, 4);
        assert!(!store.staging_path(&observed.jobs[0].id).exists());
        assert!(DirectoryLease::acquire(store.lease.root()).is_err());
    }

    #[test]
    fn generated_orphans_are_quarantined_and_charged_on_each_restart_without_content_identity_claims()
     {
        let directory = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(directory.path().join("artifacts")).unwrap();
        let filename = format!("job-{}.part", "b".repeat(32));
        fs::write(lease.root().join(&filename), b"retained orphan").unwrap();
        let limits = ArtifactLimits {
            maximum_artifact_bytes: 16,
            maximum_total_bytes: 16,
            ..ArtifactLimits::default()
        };
        let store = ArtifactStore::open(
            lease.clone(),
            limits.clone(),
            SourcePolicy::AllowLoopbackHttp,
        )
        .unwrap();
        let observed = store.inspect().unwrap();
        assert_eq!(observed.charged_bytes, 16);
        assert!(observed.jobs.is_empty());
        assert!(observed.artifacts.is_empty());
        assert_eq!(observed.quarantined_files[0].filename, filename);
        assert_eq!(
            store
                .begin(None, "a".repeat(64), Some(4), 4, 0)
                .err()
                .unwrap()
                .code(),
            "artifact_limit"
        );
        drop(store);
        let restarted = ArtifactStore::open(lease, limits, SourcePolicy::Offline).unwrap();
        assert_eq!(restarted.inspect().unwrap().charged_bytes, 16);
        assert_eq!(
            fs::read(restarted.lease.root().join(filename)).unwrap(),
            b"retained orphan"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lost_neighbor_blocks_resumed_truncate_write_publish_and_cleanup_after_cheap_checks() {
        let (directory, store) = scoped_store();
        let (job, _) = store
            .begin(
                Some("http://127.0.0.1:1/file".into()),
                format!("{:x}", Sha256::digest(b"safe")),
                Some(4),
                4,
                0,
            )
            .unwrap();
        fs::write(store.staging_path(&job.id), b"safe").unwrap();
        store.set_state(&job.id, JobState::Paused, None).unwrap();
        store.runtime.lock().unwrap().active.remove(&job.id);
        let paused = store.job(&job.id).unwrap();
        let target = Arc::new(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(store.staging_path(&job.id))
                .unwrap(),
        );
        let manifest = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        replace_neighbor(directory.path());
        assert_eq!(
            store
                .resume(&job.id, paused.revision - 1)
                .unwrap_err()
                .code(),
            "artifact_revision_conflict"
        );
        assert_eq!(
            store.resume(&job.id, paused.revision).unwrap_err().code(),
            "storage_root_unavailable"
        );
        assert_eq!(
            store.resume(&"c".repeat(32), 1).unwrap_err().code(),
            "artifact_not_found"
        );
        let control = Arc::new(Control::new());
        assert_eq!(
            store
                .reset_target(&target, &control)
                .await
                .unwrap_err()
                .code(),
            "storage_root_unavailable"
        );
        assert_eq!(
            store
                .write_chunk(&target, &control, Bytes::from_static(b"unsafe"))
                .await
                .unwrap_err()
                .code(),
            "storage_root_unavailable"
        );
        assert_eq!(
            store
                .publish(&job.id, 4, &paused.expected_sha256, &target, &control)
                .unwrap_err()
                .code(),
            "storage_root_unavailable"
        );
        assert_eq!(
            store.cancel_finished(&job.id).unwrap_err().code(),
            "storage_root_unavailable"
        );
        assert_eq!(fs::read(store.staging_path(&job.id)).unwrap(), b"safe");
        assert!(!store.artifact_path(&paused.expected_sha256).exists());
        assert_eq!(
            fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            manifest
        );
        assert_eq!(store.inspect().unwrap().charged_bytes, 4);
        assert_eq!(
            crate::ApiError::from(ArtifactError::RootUnavailable).status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            crate::ApiError::from(ArtifactError::from(io::Error::other(
                "storage_root_unavailable"
            )))
            .status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_loss_still_stops_pause_and_cancel_writers_without_terminal_publication_or_cleanup()
     {
        for command in [PAUSE, CANCEL] {
            let source = fixture().await;
            source.source.mode.store(5, Ordering::Release);
            let (directory, store) = scoped_store();
            let job = store.start_download(request(&source), 0).unwrap();
            let partial = progress(&store, &job.id).await;
            until(|| source.source.waiting.load(Ordering::Acquire)).await;
            let stage = fs::read(store.staging_path(&job.id)).unwrap();
            let manifest = fs::read(store.lease.root().join(MANIFEST)).unwrap();
            let neighbor = replace_neighbor(directory.path());
            let refused = store
                .control(&job.id, partial.revision, command)
                .unwrap_err();
            assert_eq!(refused.code(), "storage_root_unavailable");
            let interrupted = settled(&store, &job.id).await;
            assert_eq!(interrupted.state, JobState::Interrupted);
            assert_eq!(
                interrupted.error.as_deref(),
                Some("storage_root_unavailable")
            );
            assert!(!interrupted.writer_active);
            assert_eq!(fs::read(store.staging_path(&job.id)).unwrap(), stage);
            assert_eq!(
                fs::read(store.lease.root().join(MANIFEST)).unwrap(),
                manifest
            );
            assert_eq!(
                store.inspect().unwrap().charged_bytes,
                source.source.bytes.len() as u64
            );
            assert_eq!(
                store
                    .resume(&job.id, interrupted.revision)
                    .unwrap_err()
                    .code(),
                "storage_root_unavailable"
            );
            assert_eq!(source.source.ranges.lock().unwrap().len(), 1);
            assert_eq!(
                fs::read(neighbor.join("foreign-canary")).unwrap(),
                b"foreign bytes stay unchanged"
            );
            restore_neighbor(directory.path());
            if command == PAUSE {
                source.source.mode.store(0, Ordering::Release);
                store.resume(&job.id, interrupted.revision).unwrap();
                let completed = settled(&store, &job.id).await;
                assert_eq!(completed.state, JobState::Complete, "{completed:?}");
                assert_eq!(
                    source.source.ranges.lock().unwrap()[1].0,
                    Some(format!("bytes={}-", partial.downloaded_bytes))
                );
            } else {
                assert_eq!(
                    store.cancel(&job.id, interrupted.revision).unwrap().state,
                    JobState::Cancelled
                );
                assert_eq!(store.inspect().unwrap().charged_bytes, 0);
            }
            assert_eq!(
                fs::read(directory.path().join("foreign-neighbor/foreign-canary")).unwrap(),
                b"foreign bytes stay unchanged"
            );
        }
    }

    fn request(source: &HttpFixture) -> DownloadRequest {
        DownloadRequest {
            source_url: format!("{}/file", source.base),
            expected_sha256: format!("{:x}", Sha256::digest(source.source.bytes.as_slice())),
            expected_bytes: Some(source.source.bytes.len() as u64),
            maximum_bytes: source.source.bytes.len() as u64,
        }
    }

    async fn progress(store: &ArtifactStore, id: &str) -> JobView {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let view = store.job(id).unwrap();
                if view.downloaded_bytes > 0 {
                    return view;
                }
                assert!(
                    view.writer_active,
                    "job settled before partial progress: {view:?}"
                );
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("download did not make bounded progress")
    }

    pub(super) async fn settled(store: &ArtifactStore, id: &str) -> JobView {
        tokio::time::timeout(Duration::from_secs(5), store.wait(id))
            .await
            .expect("artifact job did not settle")
            .unwrap()
    }

    #[tokio::test]
    async fn artifact_release_does_not_wait_for_manifest_ownership_and_reuses_its_counter() {
        let source = fixture().await;
        let (_directory, store) = store();
        let job = store.start_download(request(&source), 0).unwrap();
        let complete = settled(&store, &job.id).await;
        assert_eq!(complete.state, JobState::Complete);
        let id = complete.artifact.unwrap();
        assert!(
            store.runtime.lock().unwrap().manifest.artifacts[&id]
                .holds
                .is_none()
        );
        let durable = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let first = store.hold(&id).unwrap();
        let second = store.hold(&id).unwrap();
        assert!(Arc::ptr_eq(
            &first._hold.as_ref().unwrap().0,
            &second._hold.as_ref().unwrap().0
        ));
        assert_eq!(
            fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            durable
        );
        store
            .reference(&id, "counter-publication", true, store.generation())
            .unwrap();
        let retained = store.runtime.lock().unwrap().manifest.artifacts[&id]
            .holds
            .clone()
            .unwrap();
        assert!(Arc::ptr_eq(&retained, &first._hold.as_ref().unwrap().0));
        assert_eq!(retained.load(Ordering::Acquire), 2);
        let held_manifest = store.runtime.lock().unwrap();
        let (released, observed) = std::sync::mpsc::channel();
        let release = std::thread::spawn(move || {
            drop(second);
            released.send(()).unwrap();
        });
        let completed_without_manifest = observed.recv_timeout(Duration::from_secs(1));
        let count_while_manifest_held = retained.load(Ordering::Acquire);
        drop(held_manifest);
        release.join().unwrap();
        completed_without_manifest.expect("lease release waited for manifest ownership");
        assert_eq!(count_while_manifest_held, 1);
        assert_eq!(
            store.remove(&id, store.generation()).unwrap_err().code(),
            "artifact_busy"
        );
        drop(first);
        for _ in 0..128 {
            let next = store.hold(&id).unwrap();
            assert!(Arc::ptr_eq(&retained, &next._hold.as_ref().unwrap().0));
            assert_eq!(store.inspect().unwrap().artifacts[0].active_holds, 1);
            drop(next);
        }
        assert_eq!(store.inspect().unwrap().artifacts[0].active_holds, 0);
        store
            .reference(&id, "counter-publication", false, store.generation())
            .unwrap();
        let weak = Arc::downgrade(&retained);
        drop(retained);
        store.remove(&id, store.generation()).unwrap();
        assert!(weak.upgrade().is_none());
        assert!(store.inspect().unwrap().artifacts.is_empty());
    }

    #[tokio::test]
    async fn download_publishes_only_validated_complete_bytes_and_references_guard_removal() {
        let source = fixture().await;
        let (_directory, store) = store();
        let job = store.start_download(request(&source), 0).unwrap();
        assert!(store.inspect().unwrap().artifacts.is_empty());
        let job = settled(&store, &job.id).await;
        assert_eq!(job.state, JobState::Complete, "{job:?}");
        let id = job.artifact.unwrap();
        let held = store.hold(&id).unwrap();
        let (ready, ready_receiver) = std::sync::mpsc::channel();
        let (release, release_receiver) = std::sync::mpsc::channel();
        let worker_store = store.clone();
        let worker_id = id.clone();
        let worker = std::thread::spawn(move || {
            let held = worker_store.hold(&worker_id).unwrap();
            ready.send(()).unwrap();
            release_receiver.recv().unwrap();
            drop(held);
        });
        ready_receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(fs::read(held.path()).unwrap(), *source.source.bytes);
        assert_eq!(store.inspect().unwrap().artifacts[0].active_holds, 2);
        assert_eq!(
            store.remove(&id, store.generation()).unwrap_err().code(),
            "artifact_busy"
        );
        store
            .reference(&id, "registered-model", true, store.generation())
            .unwrap();
        drop(held);
        assert_eq!(
            store.remove(&id, store.generation()).unwrap_err().code(),
            "artifact_busy"
        );
        store
            .reference(&id, "registered-model", false, store.generation())
            .unwrap();
        assert_eq!(
            store.remove(&id, store.generation()).unwrap_err().code(),
            "artifact_busy"
        );
        release.send(()).unwrap();
        worker.join().unwrap();
        store.remove(&id, store.generation()).unwrap();
        assert_eq!(store.inspect().unwrap().charged_bytes, 0);
    }

    #[tokio::test]
    async fn pause_resume_uses_range_and_if_range_without_restarting_valid_partial_bytes() {
        let source = fixture().await;
        let (_directory, store) = store();
        let job = store.start_download(request(&source), 0).unwrap();
        let partial = progress(&store, &job.id).await;
        store.pause(&job.id, partial.revision).unwrap();
        let paused = settled(&store, &job.id).await;
        assert_eq!(paused.state, JobState::Paused);
        assert!(
            paused.downloaded_bytes > 0
                && paused.downloaded_bytes < source.source.bytes.len() as u64
        );
        assert_eq!(
            store.inspect().unwrap().charged_bytes,
            source.source.bytes.len() as u64
        );
        store.resume(&job.id, paused.revision).unwrap();
        let complete = settled(&store, &job.id).await;
        assert_eq!(complete.state, JobState::Complete, "{complete:?}");
        let ranges = source.source.ranges.lock().unwrap();
        assert_eq!(
            ranges[1].0,
            Some(format!("bytes={}-", paused.downloaded_bytes))
        );
        assert_eq!(ranges[1].1.as_deref(), Some("\"fixture-v1\""));
    }

    #[tokio::test]
    async fn cancel_confirms_writer_settlement_before_removing_partial_file_and_releasing_charge() {
        let source = fixture().await;
        let (_directory, store) = store();
        let job = store.start_download(request(&source), 0).unwrap();
        let partial = progress(&store, &job.id).await;
        store.cancel(&job.id, partial.revision).unwrap();
        let cancelled = settled(&store, &job.id).await;
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(!cancelled.writer_active);
        assert!(!store.staging_path(&job.id).exists());
        assert!(store.inspect().unwrap().artifacts.is_empty());
        assert_eq!(store.inspect().unwrap().charged_bytes, 0);
    }

    #[tokio::test]
    async fn paused_writer_can_cancel_without_resuming_network_work() {
        let source = fixture().await;
        let (_directory, store) = store();
        let job = store.start_download(request(&source), 0).unwrap();
        let partial = progress(&store, &job.id).await;
        store.pause(&job.id, partial.revision).unwrap();
        let paused = settled(&store, &job.id).await;
        let requests = source.source.ranges.lock().unwrap().len();
        let cancelled = store.cancel(&job.id, paused.revision).unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(!cancelled.writer_active);
        assert_eq!(source.source.ranges.lock().unwrap().len(), requests);
        assert!(!store.staging_path(&job.id).exists());
        assert_eq!(store.inspect().unwrap().charged_bytes, 0);
    }

    #[test]
    fn publication_uncertainty_survives_cancellation_and_restart_without_releasing_bytes() {
        let (_directory, store) = store();
        let (job, _) = store.begin(None, "1".repeat(64), Some(4), 4, 0).unwrap();
        fs::write(store.staging_path(&job.id), b"data").unwrap();
        store
            .set_state(&job.id, JobState::Publishing, None)
            .unwrap();
        let target = store.artifact_path(&"1".repeat(64));
        fs::hard_link(store.staging_path(&job.id), &target).unwrap();
        fs::remove_file(store.staging_path(&job.id)).unwrap();
        store
            .settle_transfer(&job.id, CANCEL, Err(ArtifactError::Corrupt))
            .unwrap();
        store.runtime.lock().unwrap().active.remove(&job.id);
        let inspection = store.inspect().unwrap();
        assert_eq!(inspection.charged_bytes, 4);
        assert_eq!(inspection.jobs[0].state, JobState::Interrupted);
        assert_eq!(
            inspection.jobs[0].error.as_deref(),
            Some("artifact_publication_uncertain")
        );
        assert!(inspection.artifacts.is_empty());
        assert_eq!(fs::read(&target).unwrap(), b"data");
        let lease = store.lease.clone();
        drop(store);
        let reopened =
            ArtifactStore::open(lease, ArtifactLimits::default(), SourcePolicy::Offline).unwrap();
        let observed = reopened.job(&job.id).unwrap();
        assert_eq!(reopened.inspect().unwrap().charged_bytes, 4);
        assert_eq!(
            reopened
                .cancel(&job.id, observed.revision)
                .unwrap_err()
                .code(),
            "artifact_revision_conflict"
        );
        assert!(fs::read(&target).is_ok());
    }

    #[tokio::test]
    async fn active_transfer_limit_rejects_concurrent_work_and_releases_after_settlement() {
        let source = fixture().await;
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(
            DirectoryLease::acquire(directory.path().join("store")).unwrap(),
            ArtifactLimits {
                maximum_active_jobs: 1,
                ..ArtifactLimits::default()
            },
            SourcePolicy::AllowLoopbackHttp,
        )
        .unwrap();
        let job = store.start_download(request(&source), 0).unwrap();
        assert_eq!(
            store
                .start_download(request(&source), store.generation())
                .unwrap_err()
                .code(),
            "artifact_limit"
        );
        let partial = progress(&store, &job.id).await;
        store.cancel(&job.id, partial.revision).unwrap();
        assert_eq!(settled(&store, &job.id).await.state, JobState::Cancelled);
        let next = store
            .start_download(request(&source), store.generation())
            .unwrap();
        store.cancel(&next.id, next.revision).unwrap();
        assert_eq!(settled(&store, &next.id).await.state, JobState::Cancelled);
    }

    #[tokio::test]
    async fn restart_marks_incomplete_work_interrupted_and_resumes_from_committed_source_identity()
    {
        let source = fixture().await;
        let (_directory, store) = store();
        let job = store.start_download(request(&source), 0).unwrap();
        let partial = progress(&store, &job.id).await;
        store.pause(&job.id, partial.revision).unwrap();
        let paused = settled(&store, &job.id).await;
        let lease = store.lease.clone();
        {
            let mut runtime = store.runtime.lock().unwrap();
            let mut manifest = runtime.manifest.clone();
            manifest.jobs.get_mut(&job.id).unwrap().state = JobState::Downloading;
            store.commit(&mut runtime, manifest).unwrap();
        }
        drop(store);
        let store = ArtifactStore::open(
            lease,
            ArtifactLimits::default(),
            SourcePolicy::AllowLoopbackHttp,
        )
        .unwrap();
        let interrupted = store.job(&job.id).unwrap();
        assert_eq!(interrupted.state, JobState::Interrupted);
        assert_eq!(interrupted.downloaded_bytes, paused.downloaded_bytes);
        assert_eq!(
            store.inspect().unwrap().charged_bytes,
            source.source.bytes.len() as u64
        );
        store.resume(&job.id, interrupted.revision).unwrap();
        assert_eq!(settled(&store, &job.id).await.state, JobState::Complete);
    }

    #[tokio::test]
    async fn wrong_hash_and_invalid_range_validator_quarantine_without_publication() {
        let source = fixture().await;
        let (_directory, store) = store();
        let mut wrong = request(&source);
        wrong.expected_sha256 = "0".repeat(64);
        let job = store.start_download(wrong, 0).unwrap();
        let failed = settled(&store, &job.id).await;
        assert_eq!(failed.state, JobState::Quarantined);
        assert_eq!(failed.error.as_deref(), Some("artifact_hash_mismatch"));
        assert!(store.inspect().unwrap().artifacts.is_empty());
        assert!(store.staging_path(&job.id).exists());
        let cancelled = store.cancel(&job.id, failed.revision).unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(!store.staging_path(&job.id).exists());
        assert_eq!(store.inspect().unwrap().charged_bytes, 0);
        store.forget_job(&job.id, cancelled.revision).unwrap();
        let next = store
            .start_download(request(&source), store.generation())
            .unwrap();
        let partial = progress(&store, &next.id).await;
        store.pause(&next.id, partial.revision).unwrap();
        let paused = settled(&store, &next.id).await;
        source.source.mode.store(1, Ordering::Release);
        store.resume(&next.id, paused.revision).unwrap();
        let invalid = settled(&store, &next.id).await;
        assert_eq!(invalid.state, JobState::Quarantined);
        assert_eq!(invalid.error.as_deref(), Some("artifact_source_changed"));
        assert!(store.inspect().unwrap().artifacts.is_empty());
        store.retry(&next.id, invalid.revision).unwrap();
        assert_eq!(settled(&store, &next.id).await.state, JobState::Complete);
    }

    #[tokio::test]
    async fn ignored_range_restarts_cleanly_and_failed_sources_can_retry() {
        let source = fixture().await;
        let (_directory, store) = store();
        source.source.mode.store(4, Ordering::Release);
        let job = store.start_download(request(&source), 0).unwrap();
        let failed = settled(&store, &job.id).await;
        assert_eq!(failed.state, JobState::Failed);
        source.source.mode.store(0, Ordering::Release);
        store.retry(&job.id, failed.revision).unwrap();
        let partial = progress(&store, &job.id).await;
        store.pause(&job.id, partial.revision).unwrap();
        let paused = settled(&store, &job.id).await;
        source.source.mode.store(2, Ordering::Release);
        store.resume(&job.id, paused.revision).unwrap();
        let complete = settled(&store, &job.id).await;
        assert_eq!(complete.state, JobState::Complete, "{complete:?}");
        let held = store.hold(complete.artifact.as_deref().unwrap()).unwrap();
        assert_eq!(fs::read(held.path()).unwrap(), *source.source.bytes);
    }

    #[tokio::test]
    async fn redirect_signed_urls_stay_in_memory_and_offline_policy_blocks_all_network_start() {
        let source = fixture().await;
        let (_directory, store) = store();
        let mut download = request(&source);
        download.source_url = format!("{}/redirect", source.base);
        let job = store.start_download(download, 0).unwrap();
        assert_eq!(settled(&store, &job.id).await.state, JobState::Complete);
        let manifest = fs::read_to_string(store.lease.root().join(MANIFEST)).unwrap();
        assert!(!manifest.contains("X-Amz-Signature"));
        assert!(!manifest.contains("ephemeral-test-only"));
        let directory = tempfile::tempdir().unwrap();
        let offline = ArtifactStore::open(
            DirectoryLease::acquire(directory.path().join("offline")).unwrap(),
            ArtifactLimits::default(),
            SourcePolicy::Offline,
        )
        .unwrap();
        assert_eq!(
            offline
                .start_download(request(&source), 0)
                .unwrap_err()
                .code(),
            "artifact_offline"
        );
        assert!(offline.inspect().unwrap().jobs.is_empty());
    }

    #[tokio::test]
    async fn default_source_policy_rejects_localhost_dns_private_addresses_and_redirect_downgrades()
    {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(
            DirectoryLease::acquire(directory.path().join("store")).unwrap(),
            ArtifactLimits::default(),
            SourcePolicy::PublicHttps,
        )
        .unwrap();
        assert!(matches!(
            store
                .client(&Url::parse("https://localhost/file").unwrap())
                .await,
            Err(ArtifactError::SourcePolicy)
        ));
        for url in [
            "http://127.0.0.1/file",
            "https://127.0.0.1/file",
            "https://192.168.0.1/file",
            "https://192.0.0.1/file",
            "https://100.64.0.1/file",
            "https://[::1]/file",
            "https://[::2]/file",
            "https://[fec0::1]/file",
            "https://[2002:7f00:1::1]/file",
            "https://[2001::1]/file",
            "https://[3fff::1]/file",
            "https://[::ffff:127.0.0.1]/file",
            "https://public.example/file#fragment",
            "https://user:password@public.example/file",
            "https://public.example/file?token=secret",
        ] {
            assert!(
                source_url(url, SourcePolicy::PublicHttps, false).is_err(),
                "{url}"
            );
        }
        assert!(
            source_url(
                "http://127.0.0.1/file?signature=secret",
                SourcePolicy::PublicHttps,
                true
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn authorized_file_import_keeps_bytes_immutable_and_bounded() {
        let (directory, store) = store();
        let source = directory.path().join("authorized.gguf");
        fs::write(&source, b"authorized original weights").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"authorized original weights"));
        let job = store
            .start_import(
                File::open(&source).unwrap(),
                ImportRequest {
                    expected_sha256: sha.clone(),
                    expected_bytes: Some(27),
                    maximum_bytes: 27,
                },
                0,
            )
            .unwrap();
        let complete = settled(&store, &job.id).await;
        assert_eq!(complete.state, JobState::Complete, "{complete:?}");
        assert_eq!(fs::read(&source).unwrap(), b"authorized original weights");
        let held = store.hold(&sha).unwrap();
        assert_eq!(
            fs::read(held.path()).unwrap(),
            b"authorized original weights"
        );
        assert_eq!(
            store.job("../../outside").unwrap_err().code(),
            "artifact_invalid"
        );
        assert_eq!(
            store.hold("../../outside").err().unwrap().code(),
            "artifact_invalid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn publication_rejects_staging_path_replacement_after_stream_validation() {
        use std::os::unix::fs::symlink;
        let (directory, store) = store();
        let expected = format!("{:x}", Sha256::digest(b"safe"));
        let (job, control) = store.begin(None, expected.clone(), Some(4), 4, 0).unwrap();
        let stage = store.staging_path(&job.id);
        fs::write(&stage, b"safe").unwrap();
        let validated = File::open(&stage).unwrap();
        let external = directory.path().join("external");
        fs::write(&external, b"unsafe external").unwrap();
        fs::remove_file(&stage).unwrap();
        symlink(&external, &stage).unwrap();
        assert_eq!(
            store
                .publish(&job.id, 4, &expected, &validated, &control)
                .unwrap_err()
                .code(),
            "artifact_corrupt"
        );
        assert!(store.inspect().unwrap().artifacts.is_empty());
        assert!(!store.artifact_path(&expected).exists());
        assert_eq!(fs::read(&external).unwrap(), b"unsafe external");
        assert_eq!(store.inspect().unwrap().charged_bytes, 4);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_replacement_and_reference_corruption_never_read_or_remove_external_files() {
        use std::os::unix::fs::symlink;
        let (directory, store) = store();
        let source = directory.path().join("authorized");
        fs::write(&source, b"safe weights").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"safe weights"));
        let job = store
            .start_import(
                File::open(&source).unwrap(),
                ImportRequest {
                    expected_sha256: sha.clone(),
                    expected_bytes: Some(12),
                    maximum_bytes: 12,
                },
                0,
            )
            .unwrap();
        assert_eq!(settled(&store, &job.id).await.state, JobState::Complete);
        fs::remove_file(store.artifact_path(&sha)).unwrap();
        symlink(&source, store.artifact_path(&sha)).unwrap();
        assert_eq!(store.hold(&sha).err().unwrap().code(), "artifact_corrupt");
        assert_eq!(store.inspect().unwrap().artifacts[0].active_holds, 0);
        assert_eq!(
            store.remove(&sha, store.generation()).unwrap_err().code(),
            "artifact_corrupt"
        );
        assert_eq!(fs::read(&source).unwrap(), b"safe weights");
    }

    #[tokio::test]
    async fn count_byte_and_generation_limits_reject_without_creating_unbounded_work() {
        let source = fixture().await;
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(
            DirectoryLease::acquire(directory.path().join("store")).unwrap(),
            ArtifactLimits {
                maximum_jobs: 1,
                maximum_total_bytes: source.source.bytes.len() as u64,
                ..ArtifactLimits::default()
            },
            SourcePolicy::AllowLoopbackHttp,
        )
        .unwrap();
        let job = store.start_download(request(&source), 0).unwrap();
        assert_eq!(
            store
                .start_download(request(&source), 0)
                .unwrap_err()
                .code(),
            "artifact_revision_conflict"
        );
        assert_eq!(
            store
                .start_download(request(&source), store.generation())
                .unwrap_err()
                .code(),
            "artifact_limit"
        );
        let partial = progress(&store, &job.id).await;
        store.cancel(&job.id, partial.revision).unwrap();
        assert_eq!(settled(&store, &job.id).await.state, JobState::Cancelled);
        assert_eq!(store.inspect().unwrap().charged_bytes, 0);
        assert_eq!(
            store
                .start_download(request(&source), store.generation())
                .unwrap_err()
                .code(),
            "artifact_limit"
        );
        let cancelled = store.job(&job.id).unwrap();
        assert_eq!(
            store
                .forget_job(&job.id, cancelled.revision - 1)
                .unwrap_err()
                .code(),
            "artifact_revision_conflict"
        );
        store.forget_job(&job.id, cancelled.revision).unwrap();
        assert!(matches!(store.job(&job.id), Err(ArtifactError::NotFound)));
        let replacement = store
            .start_download(request(&source), store.generation())
            .unwrap();
        let complete = settled(&store, &replacement.id).await;
        assert_eq!(complete.state, JobState::Complete);
        assert!(complete.artifact.is_some());
    }
}
