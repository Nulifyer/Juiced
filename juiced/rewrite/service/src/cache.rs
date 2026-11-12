use crate::storage::{AtomicWriteError, DirectoryLease, LiveRoots, same_file};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MANIFEST: &str = "snapshots.json";

pub(crate) mod memory;
mod page;
mod reader;
pub use page::{DEFAULT_PAGE_SIZE, OrphanPageInfo, PageInfo, PageRequest};
mod writer;
pub use reader::{BoundSnapshotRead, ReaderMetadata, ReaderRecoveryOutcome, ReaderSettlement};
pub use writer::{OperationControl, RecoveryOutcome, WriterRequest};

#[path = "snapshot_inspection.rs"]
mod snapshot_inspection;
pub use snapshot_inspection::{
    SnapshotInspectionIssue, SnapshotInspectionOptions, SnapshotInspectionStatus,
    SnapshotStoreInspection, inspect_snapshot_store,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotIdentity {
    pub execution_fingerprint: String,
    pub principal: String,
    pub credential_epoch: String,
    pub protocol: String,
    pub session_digest: String,
}

impl SnapshotIdentity {
    pub fn validate(&self) -> Result<(), CacheError> {
        let bounded = |value: &str, maximum: usize| {
            !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
        };
        if !digest_valid(&self.execution_fingerprint)
            || !digest_valid(&self.session_digest)
            || !bounded(&self.principal, 160)
            || !bounded(&self.credential_epoch, 160)
            || !bounded(&self.protocol, 64)
        {
            return Err(CacheError::Invalid(
                "Snapshot identity needs exact execution/session hashes and bounded principal, epoch and protocol",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct CacheLimits {
    pub maximum_entries: usize,
    pub maximum_snapshot_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_manifest_bytes: u64,
    pub maximum_hash_bytes: u64,
    pub inspection_timeout: Duration,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            maximum_entries: 16384,
            maximum_snapshot_bytes: 64 * 1024 * 1024 * 1024,
            maximum_total_bytes: 128 * 1024 * 1024 * 1024,
            maximum_manifest_bytes: 16 * 1024 * 1024,
            maximum_hash_bytes: 64 * 1024 * 1024 * 1024,
            inspection_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotState {
    Pending,
    Saved,
    Deleting,
    Uncertain,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    identity: SnapshotIdentity,
    revision: u64,
    state: SnapshotState,
    reserved_bytes: u64,
    bytes: Option<u64>,
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer: Option<writer::WriterBinding>,
}

#[derive(Clone, Debug, Serialize)]
struct Manifest {
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_id: Option<String>,
    generation: u64,
    entries: BTreeMap<String, Arc<Record>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    reader_intents: BTreeMap<String, Arc<reader::ReaderIntent>>,
}

struct Runtime {
    manifest: Manifest,
    manager_owner: String,
    leases: Arc<Mutex<LeaseRegistry>>,
}

// Lease Drop and exact-exit acknowledgement cannot wait for manifest I/O.
#[derive(Default)]
struct LeaseRegistry {
    active_writes: HashSet<String>,
    readers: HashMap<String, usize>,
    exit_receipts: HashMap<String, HashMap<u64, bool>>,
}

impl Runtime {
    fn writing(&self, id: &str) -> bool {
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active_writes
            .contains(id)
    }
    fn reader_count(&self, id: &str) -> usize {
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .readers
            .get(id)
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotMetadata {
    pub id: String,
    pub revision: u64,
    pub state: SnapshotState,
    pub reserved_bytes: u64,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
    pub execution_fingerprint: String,
    pub protocol: String,
    pub session_digest: String,
    pub active_readers: usize,
    pub retained_readers: Vec<ReaderMetadata>,
    pub writer_bound: bool,
    pub writer_current_owner: bool,
    pub writer_actor_present: bool,
    pub cleanup_eligible: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRow {
    pub id: String,
    pub revision: u64,
    pub state: SnapshotState,
    pub charged: u64,
    pub identity: SnapshotIdentity,
    pub busy: bool,
    pub modified_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotInspection {
    pub snapshot: SnapshotMetadata,
    pub issues: Vec<&'static str>,
    pub checksum_valid: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CacheInspection {
    pub schema_version: u32,
    pub generation: u64,
    pub read_only: bool,
    pub recovery_authorized: bool,
    pub charged_bytes: u64,
    pub snapshots: Vec<SnapshotInspection>,
    pub orphan_files: Vec<String>,
    pub issues: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<PageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orphan_page: Option<OrphanPageInfo>,
}

pub struct CacheStore {
    leases: Arc<Mutex<LeaseRegistry>>,
    store_id: String,
    lease: DirectoryLease,
    scope: Arc<LiveRoots>,
    limits: CacheLimits,
    runtime: Mutex<Runtime>,
    control_memory: Option<Arc<memory::ControlMemory>>,
}

pub struct PendingSnapshot {
    store: Arc<CacheStore>,
    id: String,
    path: PathBuf,
    revision: u64,
    identity: SnapshotIdentity,
}

impl PendingSnapshot {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl Drop for PendingSnapshot {
    fn drop(&mut self) {
        if let Ok(mut leases) = self.store.leases.lock() {
            leases.active_writes.remove(&self.id);
        }
    }
}

pub struct SnapshotReadLease {
    store: Arc<CacheStore>,
    path: PathBuf,
    metadata: SnapshotMetadata,
}

impl SnapshotReadLease {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn metadata(&self) -> &SnapshotMetadata {
        &self.metadata
    }
}

impl Drop for SnapshotReadLease {
    fn drop(&mut self) {
        if let Ok(mut leases) = self.store.leases.lock()
            && let Some(count) = leases.readers.get_mut(&self.metadata.id)
        {
            *count -= 1;
            if *count == 0 {
                leases.readers.remove(&self.metadata.id);
            }
        }
    }
}

#[derive(Debug)]
pub enum CacheError {
    RootUnavailable,
    Invalid(&'static str),
    NotFound,
    IdentityMismatch,
    Conflict,
    Busy,
    Uncertain,
    Corrupt(&'static str),
    Limit,
    Cancelled,
    Deadline,
    Io(io::Error),
    Persistence(AtomicWriteError),
}

impl CacheError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::RootUnavailable => "storage_root_unavailable",
            Self::Invalid(_) => "cache_invalid",
            Self::NotFound => "snapshot_not_found",
            Self::IdentityMismatch => "snapshot_identity_mismatch",
            Self::Conflict => "cache_revision_conflict",
            Self::Busy => "snapshot_busy",
            Self::Uncertain => "snapshot_uncertain",
            Self::Corrupt(_) => "snapshot_corrupt",
            Self::Limit => "cache_limit",
            Self::Cancelled => "state_operation_cancelled",
            Self::Deadline => "state_operation_deadline",
            Self::Io(_) => "cache_io",
            Self::Persistence(error) if error.root_unavailable() => "storage_root_unavailable",
            Self::Persistence(_) => "cache_persistence",
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::RootUnavailable => {
                "A claimed storage root is unavailable. Restore its owned path before retrying."
            }
            Self::Invalid(_) => "The snapshot request has an invalid identifier or identity.",
            Self::NotFound => "The snapshot is unavailable in the current credential namespace.",
            Self::IdentityMismatch => {
                "The snapshot belongs to a different execution, protocol or session identity. Rebuild the context cold."
            }
            Self::Conflict => {
                "The cache revision changed. Refresh and retry with the current revision."
            }
            Self::Busy => "An active writer or reader is using this snapshot.",
            Self::Uncertain => {
                "Snapshot ownership or cleanup remains uncertain. Its reservation is retained until a qualified recovery operation confirms termination."
            }
            Self::Corrupt(_) => {
                "Snapshot metadata or bytes failed validation. The retained data needs inspection."
            }
            Self::Limit => {
                "The snapshot or cache operation exceeds a configured size, count or inspection limit."
            }
            Self::Cancelled => "The state operation was cancelled before its next effect.",
            Self::Deadline => "The state operation did not complete before its accepted deadline.",
            Self::Io(_) => {
                "The snapshot storage operation failed. Inspect the managed storage location."
            }
            Self::Persistence(error) if error.root_unavailable() => {
                Self::RootUnavailable.public_message()
            }
            Self::Persistence(_) => {
                "The snapshot metadata operation did not establish full durability. Refresh the committed state before retrying."
            }
        }
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) | Self::Corrupt(reason) => {
                write!(formatter, "{}: {reason}", self.code())
            }
            Self::Io(error) => write!(formatter, "{}: {error}", self.code()),
            Self::Persistence(error) => write!(formatter, "{}: {error}", self.code()),
            _ => formatter.write_str(self.public_message()),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Persistence(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for CacheError {
    fn from(error: io::Error) -> Self {
        if crate::storage::is_root_unavailable(&error) {
            Self::RootUnavailable
        } else {
            Self::Io(error)
        }
    }
}

impl CacheStore {
    pub(crate) fn open_accounted(
        lease: DirectoryLease,
        limits: CacheLimits,
        scope: Arc<LiveRoots>,
        bootstrap: memory::Bootstrap,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let mut store = Self::open_scoped(lease, limits, scope)?;
        Arc::get_mut(&mut store)
            .expect("new cache has one owner")
            .control_memory = Some(bootstrap.retain_metadata()?);
        Ok(store)
    }

    pub(crate) fn control_memory(&self) -> Option<Arc<memory::ControlMemory>> {
        self.control_memory.clone()
    }

    pub fn open(lease: DirectoryLease) -> Result<Arc<Self>, CacheError> {
        Self::open_with_limits(lease, CacheLimits::default())
    }

    pub fn open_with_limits(
        lease: DirectoryLease,
        limits: CacheLimits,
    ) -> Result<Arc<Self>, CacheError> {
        let scope = LiveRoots::single(lease.clone())?;
        Self::open_scoped(lease, limits, scope)
    }

    pub fn open_scoped(
        lease: DirectoryLease,
        limits: CacheLimits,
        scope: Arc<LiveRoots>,
    ) -> Result<Arc<Self>, CacheError> {
        if !scope.contains(&lease) {
            return Err(CacheError::Invalid(
                "Cache root must belong to its storage scope",
            ));
        }
        if limits.maximum_entries == 0
            || limits.maximum_snapshot_bytes == 0
            || limits.maximum_total_bytes == 0
            || limits.maximum_manifest_bytes == 0
            || limits.maximum_hash_bytes == 0
            || limits.inspection_timeout.is_zero()
        {
            return Err(CacheError::Invalid("Cache limits must be positive"));
        }
        scope.verify().map_err(|_| CacheError::RootUnavailable)?;
        let candidate_store_id = crate::storage::generated_id()?;
        let manager_owner = crate::storage::generated_id()?;
        let path = lease.root().join(MANIFEST);
        let mut manifest = match regular_file(&path) {
            Ok(mut file) => {
                if file.metadata()?.len() > limits.maximum_manifest_bytes {
                    return Err(CacheError::Limit);
                }
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(limits.maximum_manifest_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > limits.maximum_manifest_bytes {
                    return Err(CacheError::Limit);
                }
                decode_manifest(&bytes, limits.maximum_entries)?
            }
            Err(CacheError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                writer::refuse_missing_manifest_journals(&lease, &limits)?;
                let manifest = Manifest {
                    schema_version: 1,
                    store_id: Some(candidate_store_id.clone()),
                    generation: 0,
                    entries: BTreeMap::new(),
                    reader_intents: BTreeMap::new(),
                };
                manifest_within_limit(&manifest, limits.maximum_manifest_bytes)?;
                scope
                    .write_json(&lease, MANIFEST, &manifest)
                    .map_err(CacheError::Persistence)?;
                manifest
            }
            Err(error) => return Err(error),
        };
        validate_manifest(
            &manifest,
            limits.maximum_entries,
            limits.maximum_snapshot_bytes,
        )?;
        writer::load_recovery_journals(&lease, &scope, &limits, &mut manifest)?;
        validate_manifest(
            &manifest,
            limits.maximum_entries,
            limits.maximum_snapshot_bytes,
        )?;
        scope.verify().map_err(|_| CacheError::RootUnavailable)?;
        let store_id = manifest.store_id.clone().unwrap_or(candidate_store_id);
        let leases = Arc::new(Mutex::new(LeaseRegistry::default()));
        Ok(Arc::new(Self {
            leases: leases.clone(),
            store_id,
            lease,
            scope,
            limits,
            runtime: Mutex::new(Runtime {
                manifest,
                manager_owner,
                leases,
            }),
            control_memory: None,
        }))
    }

    pub fn generation(&self) -> u64 {
        self.runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .manifest
            .generation
    }

    pub(crate) fn check_reservation(
        &self,
        identity: &SnapshotIdentity,
        expected_generation: u64,
        maximum_bytes: u64,
    ) -> Result<(), CacheError> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        self.reservation_guards(&runtime, identity, expected_generation, maximum_bytes)
    }

    fn reservation_guards(
        &self,
        runtime: &Runtime,
        identity: &SnapshotIdentity,
        expected_generation: u64,
        maximum_bytes: u64,
    ) -> Result<(), CacheError> {
        identity.validate()?;
        if maximum_bytes == 0 || maximum_bytes > self.limits.maximum_snapshot_bytes {
            return Err(CacheError::Limit);
        }
        if runtime.manifest.generation != expected_generation {
            return Err(CacheError::Conflict);
        }
        Ok(())
    }

    pub fn begin(
        self: &Arc<Self>,
        identity: SnapshotIdentity,
        expected_generation: u64,
        maximum_bytes: u64,
    ) -> Result<PendingSnapshot, CacheError> {
        self.begin_observed(
            identity,
            expected_generation,
            maximum_bytes,
            crate::storage::generated_id,
            |_, _| {},
        )
    }

    fn begin_observed(
        self: &Arc<Self>,
        identity: SnapshotIdentity,
        expected_generation: u64,
        maximum_bytes: u64,
        generate_id: impl FnOnce() -> io::Result<String>,
        checkpoint: impl FnMut(BeginPoint, &str),
    ) -> Result<PendingSnapshot, CacheError> {
        self.begin_impl(
            identity,
            expected_generation,
            maximum_bytes,
            None,
            generate_id,
            checkpoint,
        )
    }

    fn begin_impl(
        self: &Arc<Self>,
        identity: SnapshotIdentity,
        expected_generation: u64,
        maximum_bytes: u64,
        bound: Option<(WriterRequest, &OperationControl)>,
        generate_id: impl FnOnce() -> io::Result<String>,
        mut checkpoint: impl FnMut(BeginPoint, &str),
    ) -> Result<PendingSnapshot, CacheError> {
        let (writer, control) = match bound {
            Some((writer, control)) => (Some(writer), Some(control)),
            None => (None, None),
        };
        if let Some(control) = control {
            control.check()?;
        }
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        self.reservation_guards(&runtime, &identity, expected_generation, maximum_bytes)?;
        self.verify_scope()?;
        if runtime
            .manifest
            .entries
            .len()
            .checked_add(runtime.manifest.reader_intents.len())
            .is_none_or(|count| count >= self.limits.maximum_entries)
            || self
                .charged(&runtime.manifest)?
                .checked_add(maximum_bytes)
                .is_none_or(|total| total > self.limits.maximum_total_bytes)
        {
            return Err(CacheError::Limit);
        }
        let id = generate_id()?;
        let path = self.path(&id);
        validate_id(&id)?;
        if runtime.manifest.entries.contains_key(&id) {
            return Err(CacheError::Conflict);
        }
        let mut next = runtime.manifest.clone();
        let binding = writer
            .map(|request| {
                writer::WriterBinding::new(
                    &self.store_id,
                    &runtime.manager_owner,
                    &id,
                    &identity,
                    maximum_bytes,
                    request,
                )
            })
            .transpose()?;
        if binding.is_some() {
            next.store_id = Some(self.store_id.clone());
        }
        let reservation = || Record {
            identity: identity.clone(),
            revision: 1,
            state: SnapshotState::Pending,
            reserved_bytes: maximum_bytes,
            bytes: None,
            sha256: None,
            writer: binding.clone(),
        };
        next.entries.insert(id.clone(), Arc::new(reservation()));
        checkpoint(BeginPoint::BeforeCommit, &id);
        if let Some(control) = control {
            control.check()?;
        }
        if let Some(binding) = &binding {
            binding.register_receipt(&mut *self.leases.lock().map_err(|_| CacheError::Uncertain)?);
        }
        if let Err(error) = self.commit(&mut runtime, next) {
            if matches!(error, CacheError::Persistence(_))
                && !runtime.manifest.entries.contains_key(&id)
            {
                let mut uncertain = reservation();
                uncertain.state = SnapshotState::Uncertain;
                runtime.manifest.entries.insert(id, Arc::new(uncertain));
                if binding.is_some() {
                    runtime.manifest.store_id = Some(self.store_id.clone());
                }
            }
            if let Some(binding) = &binding {
                self.prune_exit_receipt(&runtime, binding);
            }
            return Err(error);
        }
        checkpoint(BeginPoint::AfterCommit, &id);
        if let Some(control) = control {
            control.check()?;
        }
        self.verify_scope()?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        self.leases
            .lock()
            .map_err(|_| CacheError::Uncertain)?
            .active_writes
            .insert(id.clone());
        Ok(PendingSnapshot {
            store: self.clone(),
            id,
            path,
            revision: 1,
            identity,
        })
    }

    pub fn publish(
        &self,
        pending: &PendingSnapshot,
        expected_revision: u64,
    ) -> Result<SnapshotMetadata, CacheError> {
        self.publish_inner(pending, expected_revision, None)
    }

    pub fn publish_controlled(
        &self,
        pending: &PendingSnapshot,
        expected_revision: u64,
        control: &OperationControl,
    ) -> Result<SnapshotMetadata, CacheError> {
        self.publish_inner(pending, expected_revision, Some(control))
    }

    fn publish_inner(
        &self,
        pending: &PendingSnapshot,
        expected_revision: u64,
        control: Option<&OperationControl>,
    ) -> Result<SnapshotMetadata, CacheError> {
        if let Some(control) = control {
            control.check()?;
        }
        if !std::ptr::eq(self, pending.store.as_ref()) {
            return Err(CacheError::NotFound);
        }
        let reserved = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            let record = authorized(&runtime.manifest, &pending.id, &pending.identity)?;
            if record.revision != expected_revision {
                return Err(CacheError::Conflict);
            }
            if record.state != SnapshotState::Pending || !runtime.writing(&pending.id) {
                return Err(CacheError::Uncertain);
            }
            record.reserved_bytes
        };
        self.verify_scope()?;
        let mut budget = writer::hash_budget(&self.limits, control);
        let validated = regular_file(&pending.path)?;
        let sync = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pending.path)?;
        if !same_file(&validated, &pending.path)? || !same_file(&sync, &pending.path)? {
            return Err(CacheError::Corrupt(
                "Snapshot changed before durable publication",
            ));
        }
        self.verify_scope()?;
        if let Some(control) = control {
            control.check()?;
        }
        sync.sync_all()?;
        let (bytes, sha256) = hash_file(&pending.path, reserved, &mut budget)
            .map_err(|error| writer::controlled_error(error, &budget, control))?;
        if let Ok(metadata) = validated.metadata()
            && metadata.len() == bytes
        {
            crate::identity::remember_digest(&metadata, &sha256);
        }
        if bytes == 0 {
            return Err(CacheError::Corrupt("An empty snapshot cannot be published"));
        }
        self.verify_scope()?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        let record = authorized(&runtime.manifest, &pending.id, &pending.identity)?;
        if record.revision != expected_revision {
            return Err(CacheError::Conflict);
        }
        if record.state != SnapshotState::Pending || !runtime.writing(&pending.id) {
            return Err(CacheError::Uncertain);
        }
        let mut next = runtime.manifest.clone();
        let record = Arc::make_mut(
            next.entries
                .get_mut(&pending.id)
                .expect("authorized record"),
        );
        record.revision = record.revision.checked_add(1).ok_or(CacheError::Limit)?;
        record.state = SnapshotState::Saved;
        record.bytes = Some(bytes);
        record.sha256 = Some(sha256);
        if let Some(control) = control {
            control.check()?;
        }
        self.commit(&mut runtime, next)?;
        self.leases
            .lock()
            .map_err(|_| CacheError::Uncertain)?
            .active_writes
            .remove(&pending.id);
        self.prune_published_receipt(&runtime, &pending.id);
        Ok(metadata(
            &pending.id,
            runtime
                .manifest
                .entries
                .get(&pending.id)
                .expect("published snapshot"),
            &runtime,
        ))
    }

    pub fn restore(
        self: &Arc<Self>,
        id: &str,
        identity: &SnapshotIdentity,
    ) -> Result<SnapshotReadLease, CacheError> {
        self.restore_inner(id, identity, None)
    }

    pub fn restore_controlled(
        self: &Arc<Self>,
        id: &str,
        identity: &SnapshotIdentity,
        control: &OperationControl,
    ) -> Result<SnapshotReadLease, CacheError> {
        self.restore_inner(id, identity, Some(control))
    }

    fn restore_inner(
        self: &Arc<Self>,
        id: &str,
        identity: &SnapshotIdentity,
        control: Option<&OperationControl>,
    ) -> Result<SnapshotReadLease, CacheError> {
        if let Some(control) = control {
            control.check()?;
        }
        validate_id(id)?;
        identity.validate()?;
        let metadata = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            let record = authorized(&runtime.manifest, id, identity)?;
            if effective_state(id, record, &runtime) != SnapshotState::Saved {
                return Err(CacheError::Uncertain);
            }
            let metadata = metadata(id, record, &runtime);
            *self
                .leases
                .lock()
                .map_err(|_| CacheError::Uncertain)?
                .readers
                .entry(id.to_owned())
                .or_default() += 1;
            metadata
        };
        let held = SnapshotReadLease {
            store: self.clone(),
            path: self.path(id),
            metadata,
        };
        self.verify_scope()?;
        let mut budget = writer::hash_budget(&self.limits, control);
        let observed = fs::symlink_metadata(&held.path)
            .ok()
            .filter(|metadata| metadata.file_type().is_file())
            .and_then(|metadata| {
                Some((metadata.len(), crate::identity::cached_digest(&metadata)?))
            });
        let (bytes, sha256) = match observed {
            Some(observed) => observed,
            None => {
                let (bytes, sha256) =
                    hash_file(&held.path, self.limits.maximum_snapshot_bytes, &mut budget)
                        .map_err(|error| writer::controlled_error(error, &budget, control))?;
                if let Ok(metadata) = fs::symlink_metadata(&held.path)
                    && metadata.len() == bytes
                {
                    crate::identity::remember_digest(&metadata, &sha256);
                }
                (bytes, sha256)
            }
        };
        if held.metadata.bytes != Some(bytes) || held.metadata.sha256.as_ref() != Some(&sha256) {
            return Err(CacheError::Corrupt("Snapshot size or checksum changed"));
        }
        self.verify_scope()?;
        if let Some(control) = control {
            control.check()?;
        }
        Ok(held)
    }

    pub fn forget(
        &self,
        id: &str,
        identity: &SnapshotIdentity,
        expected_revision: u64,
    ) -> Result<(), CacheError> {
        validate_id(id)?;
        identity.validate()?;
        {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            let record = authorized(&runtime.manifest, id, identity)?;
            if record.revision != expected_revision {
                return Err(CacheError::Conflict);
            }
            if runtime.writing(id)
                || runtime.reader_count(id) > 0
                || runtime
                    .manifest
                    .reader_intents
                    .values()
                    .any(|reader| reader.snapshot == id)
            {
                return Err(CacheError::Busy);
            }
            if effective_state(id, record, &runtime) != SnapshotState::Saved {
                return Err(CacheError::Uncertain);
            }
            let mut next = runtime.manifest.clone();
            let record = Arc::make_mut(next.entries.get_mut(id).expect("authorized record"));
            record.revision = record.revision.checked_add(1).ok_or(CacheError::Limit)?;
            record.state = SnapshotState::Deleting;
            self.commit(&mut runtime, next)?;
        }
        let path = self.path(id);
        regular_file(&path)?;
        self.verify_scope()?;
        fs::remove_file(path)?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        let mut next = runtime.manifest.clone();
        next.entries.remove(id);
        self.commit(&mut runtime, next)
    }

    pub fn inventory(
        &self,
        principal: &str,
        credential_epoch: &str,
    ) -> Result<(u64, Vec<SnapshotMetadata>), CacheError> {
        self.lease.verify()?;
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        Ok((
            runtime.manifest.generation,
            metadata_rows(&runtime, Some((principal, credential_epoch))),
        ))
    }

    pub fn inventory_page(
        &self,
        principal: &str,
        credential_epoch: &str,
        request: &PageRequest,
    ) -> Result<(u64, Vec<SnapshotMetadata>, PageInfo), CacheError> {
        request.validate(self.limits.maximum_entries, false)?;
        self.lease.verify()?;
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        let (rows, page) = page::rows(&runtime, Some((principal, credential_epoch)), request)?;
        Ok((runtime.manifest.generation, rows, page))
    }

    pub fn list(
        &self,
        principal: &str,
        credential_epoch: &str,
    ) -> Result<Vec<SnapshotMetadata>, CacheError> {
        self.inventory(principal, credential_epoch)
            .map(|(_, rows)| rows)
    }

    pub(crate) fn observation(
        &self,
        id: &str,
        principal: &str,
        credential_epoch: &str,
    ) -> Result<SnapshotMetadata, CacheError> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        let record = runtime
            .manifest
            .entries
            .get(id)
            .filter(|record| {
                record.identity.principal == principal
                    && record.identity.credential_epoch == credential_epoch
            })
            .ok_or(CacheError::NotFound)?;
        Ok(metadata(id, record, &runtime))
    }

    pub fn inspect(&self) -> Result<CacheInspection, CacheError> {
        self.inspect_selected(None)
    }

    pub fn inspect_page(&self, request: &PageRequest) -> Result<CacheInspection, CacheError> {
        request.validate(self.limits.maximum_entries, true)?;
        self.inspect_selected(Some(request))
    }

    fn inspect_selected(
        &self,
        request: Option<&PageRequest>,
    ) -> Result<CacheInspection, CacheError> {
        self.inspect_observed(request, || {})
    }

    fn inspect_observed(
        &self,
        request: Option<&PageRequest>,
        checkpoint: impl FnOnce(),
    ) -> Result<CacheInspection, CacheError> {
        self.lease.verify()?;
        let (generation, charges, records, page) = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            let (rows, page) = match request {
                Some(request) => {
                    let (rows, page) = page::rows(&runtime, None, request)?;
                    (rows, Some(page))
                }
                None => (metadata_rows(&runtime, None), None),
            };
            let charges = runtime
                .manifest
                .entries
                .iter()
                .map(|(id, record)| (id.clone(), record_charge(record, 0)))
                .collect::<BTreeMap<_, _>>();
            (runtime.manifest.generation, charges, rows, page)
        };
        checkpoint();
        let mut inspection = CacheInspection {
            schema_version: 1,
            generation,
            read_only: true,
            recovery_authorized: false,
            charged_bytes: self.charged_entries(
                charges.iter().map(|(id, charge)| (id.as_str(), *charge)),
                |id| charges.contains_key(id),
            )?,
            snapshots: Vec::with_capacity(records.len()),
            orphan_files: Vec::new(),
            issues: Vec::new(),
            page,
            orphan_page: None,
        };
        let mut budget = HashBudget::new(&self.limits);
        for snapshot in records {
            let mut result = SnapshotInspection {
                snapshot,
                issues: Vec::new(),
                checksum_valid: None,
            };
            if result.snapshot.state != SnapshotState::Saved {
                result.issues.push("unresolved_snapshot_operation");
            } else {
                match hash_file(
                    &self.path(&result.snapshot.id),
                    self.limits.maximum_snapshot_bytes,
                    &mut budget,
                ) {
                    Ok((bytes, hash)) => {
                        let valid = result.snapshot.bytes == Some(bytes)
                            && result.snapshot.sha256.as_ref() == Some(&hash);
                        result.checksum_valid = Some(valid);
                        if !valid {
                            result.issues.push("snapshot_checksum_mismatch");
                        }
                    }
                    Err(CacheError::Limit) => result.issues.push("inspection_limit"),
                    Err(_) => result.issues.push("snapshot_file_unavailable_or_invalid"),
                }
            }
            inspection.snapshots.push(result);
        }
        let mut files = 0usize;
        let mut orphan_names = std::collections::BTreeSet::new();
        let mut orphan_count = 0usize;
        let mut complete = true;
        for entry in fs::read_dir(self.lease.root())? {
            files += 1;
            if files
                > self
                    .limits
                    .maximum_entries
                    .saturating_mul(2)
                    .saturating_add(16)
                || Instant::now() > budget.deadline
            {
                inspection.issues.push("inspection_limit");
                complete = false;
                break;
            }
            let entry = entry?;
            let name = entry.file_name();
            if let Some(name) = name.to_str()
                && let Some(id) = name
                    .strip_prefix("kv-")
                    .and_then(|name| name.strip_suffix(".bin"))
                && !charges.contains_key(id)
            {
                orphan_count += 1;
                if let Some(request) = request {
                    if name.len() > 4096 {
                        if !inspection.issues.contains(&"orphan_name_limit") {
                            inspection.issues.push("orphan_name_limit");
                        }
                        complete = false;
                    } else if request
                        .orphan_after
                        .as_deref()
                        .is_none_or(|after| name > after)
                    {
                        orphan_names.insert(name.to_owned());
                        if orphan_names.len() > request.limit().saturating_add(1) {
                            orphan_names.pop_last();
                        }
                    }
                } else {
                    inspection.orphan_files.push(name.to_owned());
                }
            }
        }
        if let Some(request) = request {
            let more = orphan_names.len() > request.limit();
            if more {
                orphan_names.pop_last();
            }
            inspection.orphan_files = orphan_names.into_iter().collect();
            inspection.orphan_page = Some(OrphanPageInfo {
                limit: request.limit(),
                next_after: if complete && more {
                    inspection.orphan_files.last().cloned()
                } else {
                    None
                },
                observed_count: orphan_count,
                scan_complete: complete,
            });
        }
        self.lease.verify()?;
        Ok(inspection)
    }

    pub(crate) fn session_rows(
        &self,
        protocol_prefix: &str,
        filter: impl Fn(&SnapshotIdentity) -> bool,
        with_modified: bool,
    ) -> Result<Vec<SessionRow>, CacheError> {
        self.lease.verify()?;
        let mut rows = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            runtime
                .manifest
                .entries
                .iter()
                .filter(|(_, record)| {
                    record.identity.protocol.starts_with(protocol_prefix)
                        && filter(&record.identity)
                })
                .map(|(id, record)| SessionRow {
                    id: id.clone(),
                    revision: record.revision,
                    state: effective_state(id, record, &runtime),
                    charged: record_charge(record, 0),
                    identity: record.identity.clone(),
                    busy: runtime.writing(id)
                        || runtime.reader_count(id) > 0
                        || runtime
                            .manifest
                            .reader_intents
                            .values()
                            .any(|reader| reader.snapshot == *id),
                    modified_ms: None,
                })
                .collect::<Vec<_>>()
        };
        if with_modified {
            for row in &mut rows {
                row.modified_ms = fs::symlink_metadata(self.path(&row.id))
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_millis().min(u64::MAX as u128) as u64);
            }
        }
        Ok(rows)
    }

    fn path(&self, id: &str) -> PathBuf {
        self.lease.root().join(format!("kv-{id}.bin"))
    }

    fn charged(&self, manifest: &Manifest) -> Result<u64, CacheError> {
        self.charged_entries(
            manifest
                .entries
                .iter()
                .map(|(id, record)| (id.as_str(), record_charge(record, 0))),
            |id| manifest.entries.contains_key(id),
        )
    }

    fn charged_entries<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a str, u64)>,
        known: impl Fn(&str) -> bool,
    ) -> Result<u64, CacheError> {
        let mut total = 0u64;
        for (id, minimum) in entries {
            let actual = fs::symlink_metadata(self.path(id))
                .ok()
                .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let charge = minimum.max(actual);
            total = total.checked_add(charge).ok_or(CacheError::Limit)?;
        }
        for (index, entry) in fs::read_dir(self.lease.root())?.enumerate() {
            if index
                > self
                    .limits
                    .maximum_entries
                    .saturating_mul(2)
                    .saturating_add(16)
            {
                return Err(CacheError::Limit);
            }
            let entry = entry?;
            let name = entry.file_name();
            if let Some(name) = name.to_str()
                && let Some(id) = name
                    .strip_prefix("kv-")
                    .and_then(|name| name.strip_suffix(".bin"))
                && !known(id)
            {
                let metadata = fs::symlink_metadata(entry.path())?;
                if metadata.is_file() && !metadata.file_type().is_symlink() {
                    total = total.checked_add(metadata.len()).ok_or(CacheError::Limit)?;
                }
            }
        }
        Ok(total)
    }

    fn commit(&self, runtime: &mut Runtime, mut next: Manifest) -> Result<(), CacheError> {
        next.generation = runtime
            .manifest
            .generation
            .checked_add(1)
            .ok_or(CacheError::Limit)?;
        manifest_within_limit(&next, self.limits.maximum_manifest_bytes)?;
        match self.scope.write_json(&self.lease, MANIFEST, &next) {
            Ok(_) => {
                runtime.manifest = next;
                Ok(())
            }
            Err(error) => {
                if error.committed() {
                    runtime.manifest = next;
                }
                Err(CacheError::Persistence(error))
            }
        }
    }

    fn verify_scope(&self) -> Result<(), CacheError> {
        self.scope.verify().map_err(|_| CacheError::RootUnavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BeginPoint {
    BeforeCommit,
    AfterCommit,
}

fn effective_state(id: &str, record: &Record, runtime: &Runtime) -> SnapshotState {
    if (record.state == SnapshotState::Pending && !runtime.writing(id))
        || record.state == SnapshotState::Deleting
    {
        SnapshotState::Uncertain
    } else {
        record.state
    }
}

fn record_charge(record: &Record, actual: u64) -> u64 {
    if record.state == SnapshotState::Saved {
        record.bytes.unwrap_or(record.reserved_bytes).max(actual)
    } else {
        record.reserved_bytes.max(actual)
    }
}

fn metadata(id: &str, record: &Record, runtime: &Runtime) -> SnapshotMetadata {
    let mut row = base_metadata(id, record, runtime);
    row.retained_readers = reader::metadata_for(runtime, id);
    row
}

fn metadata_rows(runtime: &Runtime, namespace: Option<(&str, &str)>) -> Vec<SnapshotMetadata> {
    let mut rows = runtime
        .manifest
        .entries
        .iter()
        .filter(|(_, record)| {
            namespace.is_none_or(|(principal, epoch)| {
                record.identity.principal == principal && record.identity.credential_epoch == epoch
            })
        })
        .map(|(id, record)| base_metadata(id, record, runtime))
        .collect::<Vec<_>>();
    for (id, intent) in &runtime.manifest.reader_intents {
        if let Ok(index) = rows.binary_search_by(|row| row.id.as_str().cmp(&intent.snapshot)) {
            rows[index]
                .retained_readers
                .push(reader::project(runtime, id, intent));
        }
    }
    rows
}

fn base_metadata(id: &str, record: &Record, runtime: &Runtime) -> SnapshotMetadata {
    SnapshotMetadata {
        id: id.to_owned(),
        revision: record.revision,
        state: effective_state(id, record, runtime),
        reserved_bytes: record.reserved_bytes,
        bytes: record.bytes,
        sha256: record.sha256.clone(),
        execution_fingerprint: record.identity.execution_fingerprint.clone(),
        protocol: record.identity.protocol.clone(),
        session_digest: record.identity.session_digest.clone(),
        active_readers: runtime.reader_count(id),
        retained_readers: Vec::new(),
        writer_bound: record.writer.is_some(),
        writer_current_owner: record
            .writer
            .as_ref()
            .is_some_and(|writer| writer.current_owner(&runtime.manager_owner)),
        writer_actor_present: record
            .writer
            .as_ref()
            .is_some_and(|writer| writer.actor_present()),
        cleanup_eligible: record
            .writer
            .as_ref()
            .is_some_and(|writer| writer.cleanup_eligible()),
    }
}

fn authorized<'a>(
    manifest: &'a Manifest,
    id: &str,
    identity: &SnapshotIdentity,
) -> Result<&'a Record, CacheError> {
    let record = manifest
        .entries
        .get(id)
        .filter(|record| {
            record.identity.principal == identity.principal
                && record.identity.credential_epoch == identity.credential_epoch
        })
        .ok_or(CacheError::NotFound)?;
    if record.identity != *identity {
        return Err(CacheError::IdentityMismatch);
    }
    Ok(record)
}

fn validate_id(id: &str) -> Result<(), CacheError> {
    if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(CacheError::Invalid(
            "Snapshot IDs are generated opaque identifiers",
        ))
    }
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_manifest(
    manifest: &Manifest,
    maximum_entries: usize,
    maximum_snapshot_bytes: u64,
) -> Result<(), CacheError> {
    if manifest.schema_version != 1
        || manifest
            .entries
            .len()
            .checked_add(manifest.reader_intents.len())
            .is_none_or(|count| count > maximum_entries)
    {
        return Err(CacheError::Corrupt(
            "The cache manifest schema or entry count is invalid",
        ));
    }
    if manifest
        .store_id
        .as_ref()
        .is_some_and(|id| !writer::opaque_valid(id))
    {
        return Err(CacheError::Corrupt("Cache store identity is invalid"));
    }
    for (id, record) in &manifest.entries {
        validate_record(
            manifest.store_id.as_deref(),
            id,
            record,
            maximum_snapshot_bytes,
        )?;
    }
    for (id, intent) in &manifest.reader_intents {
        intent.validate(manifest, id)?;
    }
    Ok(())
}

fn validate_record(
    store_id: Option<&str>,
    id: &str,
    record: &Record,
    maximum_snapshot_bytes: u64,
) -> Result<(), CacheError> {
    validate_id(id)?;
    if let Some(binding) = &record.writer {
        binding.validate(store_id, id, record)?;
    }
    record.identity.validate()?;
    if record.revision == 0
        || record.reserved_bytes == 0
        || record.reserved_bytes > maximum_snapshot_bytes
        || record
            .bytes
            .is_some_and(|bytes| bytes > record.reserved_bytes)
        || (record.state == SnapshotState::Saved
            && (record.bytes.is_none()
                || !record
                    .sha256
                    .as_ref()
                    .is_some_and(|hash| digest_valid(hash))))
    {
        return Err(CacheError::Corrupt(
            "A cache record has invalid bounds or publication metadata",
        ));
    }
    Ok(())
}

fn decode_manifest(bytes: &[u8], maximum_entries: usize) -> Result<Manifest, CacheError> {
    decode_manifest_checked(bytes, maximum_entries, &|| Ok(()))
}

fn decode_manifest_checked(
    bytes: &[u8],
    maximum_entries: usize,
    check: &dyn Fn() -> Result<(), CacheError>,
) -> Result<Manifest, CacheError> {
    use serde::de::{DeserializeSeed, Error, MapAccess, Visitor};
    struct ManifestSeed<'a> {
        maximum: usize,
        check: &'a dyn Fn() -> Result<(), CacheError>,
    }
    struct EntriesSeed<'a> {
        maximum: usize,
        check: &'a dyn Fn() -> Result<(), CacheError>,
    }
    impl<'de> Visitor<'de> for EntriesSeed<'_> {
        type Value = BTreeMap<String, Arc<Record>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("bounded unique snapshot records")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut entries = BTreeMap::new();
            loop {
                (self.check)().map_err(|_| M::Error::custom("Manifest inspection stopped"))?;
                let Some(id) = map.next_key::<String>()? else {
                    break;
                };
                if entries.len() >= self.maximum || validate_id(&id).is_err() {
                    return Err(M::Error::custom("Invalid snapshot record count or ID"));
                }
                if entries.contains_key(&id) {
                    return Err(M::Error::custom("Duplicate snapshot ID"));
                }
                entries.insert(id, Arc::new(map.next_value::<Record>()?));
            }
            Ok(entries)
        }
    }
    impl<'de> DeserializeSeed<'de> for EntriesSeed<'_> {
        type Value = BTreeMap<String, Arc<Record>>;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_map(self)
        }
    }
    struct ReadersSeed<'a> {
        maximum: usize,
        check: &'a dyn Fn() -> Result<(), CacheError>,
    }
    impl<'de> Visitor<'de> for ReadersSeed<'_> {
        type Value = BTreeMap<String, Arc<reader::ReaderIntent>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("bounded unique cache reader intents")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut readers = BTreeMap::new();
            loop {
                (self.check)().map_err(|_| M::Error::custom("Manifest inspection stopped"))?;
                let Some(id) = map.next_key::<String>()? else {
                    break;
                };
                if readers.len() >= self.maximum
                    || validate_id(&id).is_err()
                    || readers.contains_key(&id)
                {
                    return Err(M::Error::custom("Invalid reader count or ID"));
                }
                readers.insert(id, Arc::new(map.next_value::<reader::ReaderIntent>()?));
            }
            Ok(readers)
        }
    }
    impl<'de> DeserializeSeed<'de> for ReadersSeed<'_> {
        type Value = BTreeMap<String, Arc<reader::ReaderIntent>>;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_map(self)
        }
    }
    impl<'de> Visitor<'de> for ManifestSeed<'_> {
        type Value = Manifest;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a snapshot manifest")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let (mut schema_version, mut generation, mut entries, mut store_id, mut readers) =
                (None, None, None, None, None);
            loop {
                (self.check)().map_err(|_| M::Error::custom("Manifest inspection stopped"))?;
                let Some(key) = map.next_key::<String>()? else {
                    break;
                };
                match key.as_str() {
                    "schema_version" if schema_version.is_none() => {
                        schema_version = Some(map.next_value()?)
                    }
                    "generation" if generation.is_none() => generation = Some(map.next_value()?),
                    "store_id" if store_id.is_none() => {
                        store_id = Some(map.next_value::<String>()?)
                    }
                    "reader_intents" if readers.is_none() => {
                        readers = Some(map.next_value_seed(ReadersSeed {
                            maximum: self.maximum,
                            check: self.check,
                        })?);
                    }
                    "entries" if entries.is_none() => {
                        entries = Some(map.next_value_seed(EntriesSeed {
                            maximum: self.maximum,
                            check: self.check,
                        })?);
                    }
                    _ => return Err(M::Error::custom("Unknown or duplicate manifest field")),
                }
            }
            Ok(Manifest {
                reader_intents: readers.unwrap_or_default(),
                store_id,
                schema_version: schema_version
                    .ok_or_else(|| M::Error::missing_field("schema_version"))?,
                generation: generation.ok_or_else(|| M::Error::missing_field("generation"))?,
                entries: entries.ok_or_else(|| M::Error::missing_field("entries"))?,
            })
        }
    }
    impl<'de> DeserializeSeed<'de> for ManifestSeed<'_> {
        type Value = Manifest;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_map(self)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let manifest = ManifestSeed {
        maximum: maximum_entries,
        check,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| CacheError::Corrupt("The cache manifest is unreadable"))?;
    deserializer
        .end()
        .map_err(|_| CacheError::Corrupt("The cache manifest has trailing data"))?;
    Ok(manifest)
}

fn manifest_within_limit(manifest: &Manifest, maximum: u64) -> Result<(), CacheError> {
    struct BoundedCount(u64);
    impl Write for BoundedCount {
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
    let remaining = maximum.checked_sub(1).ok_or(CacheError::Limit)?;
    serde_json::to_writer(BoundedCount(remaining), manifest).map_err(|_| CacheError::Limit)
}

fn regular_file(path: &Path) -> Result<File, CacheError> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(CacheError::Corrupt(
            "Snapshot paths must be regular owned files",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() != 1 {
            return Err(CacheError::Corrupt(
                "Snapshot files cannot have external hard links",
            ));
        }
    }
    let file = File::open(path)?;
    let after = fs::symlink_metadata(path)?;
    if !after.is_file() || after.file_type().is_symlink() {
        return Err(CacheError::Corrupt("Snapshot path changed while opening"));
    }
    if !same_file(&file, path)? {
        return Err(CacheError::Corrupt("Snapshot file identity changed"));
    }
    Ok(file)
}

struct HashBudget {
    remaining: u64,
    deadline: Instant,
    cancelled: Option<Arc<AtomicBool>>,
}

impl HashBudget {
    fn new(limits: &CacheLimits) -> Self {
        Self {
            remaining: limits.maximum_hash_bytes,
            deadline: Instant::now()
                .checked_add(limits.inspection_timeout)
                .unwrap_or_else(Instant::now),
            cancelled: None,
        }
    }

    fn check(&self) -> Result<(), CacheError> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Relaxed))
            || Instant::now() >= self.deadline
        {
            Err(CacheError::Limit)
        } else {
            Ok(())
        }
    }
}

fn hash_file(
    path: &Path,
    maximum: u64,
    budget: &mut HashBudget,
) -> Result<(u64, String), CacheError> {
    let mut file = regular_file(path)?;
    let result = hash_retained(&mut file, maximum, budget)?;
    regular_file(path)?;
    if !same_file(&file, path)? {
        return Err(CacheError::Corrupt("Snapshot path changed during hashing"));
    }
    Ok(result)
}

fn hash_retained(
    file: &mut File,
    maximum: u64,
    budget: &mut HashBudget,
) -> Result<(u64, String), CacheError> {
    budget.check()?;
    let before = file.metadata()?;
    if before.len() > maximum || before.len() > budget.remaining {
        return Err(CacheError::Limit);
    }
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    while bytes < before.len() {
        budget.check()?;
        let request = (before.len() - bytes).min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..request])?;
        if count == 0 {
            break;
        }
        bytes = bytes.checked_add(count as u64).ok_or(CacheError::Limit)?;
        if bytes > maximum || count as u64 > budget.remaining {
            return Err(CacheError::Limit);
        }
        budget.remaining -= count as u64;
        hash.update(&buffer[..count]);
    }
    budget.check()?;
    let after = file.metadata()?;
    if bytes != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        return Err(CacheError::Corrupt("Snapshot changed during hashing"));
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn identity() -> SnapshotIdentity {
        SnapshotIdentity {
            execution_fingerprint: "a".repeat(64),
            principal: "configured-api".into(),
            credential_epoch: "b".repeat(64),
            protocol: "openai-chat".into(),
            session_digest: "c".repeat(64),
        }
    }

    pub(super) fn fixture() -> (tempfile::TempDir, Arc<CacheStore>) {
        let directory = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let store = CacheStore::open(lease).unwrap();
        (directory, store)
    }

    fn reopened(directory: &Path) -> Arc<CacheStore> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let lease = loop {
            match DirectoryLease::acquire(directory.join("cache")) {
                Ok(lease) => break lease,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("cache ownership did not release: {error}"),
            }
        };
        CacheStore::open(lease).unwrap()
    }

    pub(super) fn saved(store: &Arc<CacheStore>) -> SnapshotMetadata {
        let pending = store.begin(identity(), store.generation(), 1024).unwrap();
        fs::write(pending.path(), b"native KV fixture bytes").unwrap();
        store.publish(&pending, pending.revision()).unwrap()
    }

    #[cfg(unix)]
    fn scoped_fixture() -> (tempfile::TempDir, Arc<CacheStore>, Vec<DirectoryLease>) {
        let directory = tempfile::tempdir().unwrap();
        let roots = DirectoryLease::acquire_many(
            ["config", "cache", "artifacts"].map(|name| directory.path().join(name)),
        )
        .unwrap();
        let cache = roots
            .iter()
            .find(|root| root.root().ends_with("cache"))
            .unwrap()
            .clone();
        let scope = LiveRoots::new(roots.clone()).unwrap();
        let store = CacheStore::open_scoped(cache, CacheLimits::default(), scope).unwrap();
        (directory, store, roots)
    }

    #[test]
    fn durable_reservation_precedes_exclusive_creation_and_collision_retains_charge() {
        let (_directory, store) = fixture();
        let id = "1".repeat(32);
        let error = store
            .begin_observed(
                identity(),
                store.generation(),
                1024,
                || Ok(id.clone()),
                |point, id| {
                    if point == BeginPoint::AfterCommit {
                        let bytes = fs::read(store.lease.root().join(MANIFEST)).unwrap();
                        let durable =
                            decode_manifest(&bytes, store.limits.maximum_entries).unwrap();
                        assert_eq!(durable.entries[id].state, SnapshotState::Pending);
                        assert_eq!(durable.entries[id].reserved_bytes, 1024);
                        assert!(!store.path(id).exists());
                        fs::write(store.path(id), b"preexisting collision bytes").unwrap();
                    }
                },
            )
            .err()
            .unwrap();
        assert!(
            matches!(error, CacheError::Io(ref error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(error.code(), "cache_io");
        assert_eq!(
            fs::read(store.path(&id)).unwrap(),
            b"preexisting collision bytes"
        );
        let inspection = store.inspect().unwrap();
        assert_eq!(inspection.charged_bytes, 1024);
        assert_eq!(
            inspection.snapshots[0].snapshot.state,
            SnapshotState::Uncertain
        );
        assert!(!store.runtime.lock().unwrap().writing(&id));
        assert!(matches!(
            store.forget(&id, &identity(), 1),
            Err(CacheError::Uncertain)
        ));
        assert!(matches!(
            store.begin_observed(
                identity(),
                store.generation(),
                1,
                || Ok(id.clone()),
                |_, _| {}
            ),
            Err(CacheError::Conflict)
        ));
        assert_eq!(
            fs::read(store.path(&id)).unwrap(),
            b"preexisting collision bytes"
        );
    }

    #[test]
    fn failed_initial_publication_retains_uncertain_reservation_and_later_persists_it() {
        let (_directory, store) = fixture();
        let path = store.lease.root().join(MANIFEST);
        let original = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let before = store.generation();
        let id = "2".repeat(32);
        let error = store
            .begin_observed(identity(), before, 777, || Ok(id.clone()), |_, _| {})
            .err()
            .unwrap();
        assert!(
            matches!(error, CacheError::Persistence(ref error) if !error.committed() && !error.root_unavailable())
        );
        assert_eq!(error.code(), "cache_persistence");
        assert_eq!(store.generation(), before);
        assert_eq!(store.inspect().unwrap().charged_bytes, 777);
        assert_eq!(
            store
                .list(&identity().principal, &identity().credential_epoch)
                .unwrap()[0]
                .state,
            SnapshotState::Uncertain
        );
        assert!(!store.path(&id).exists());
        fs::remove_dir(&path).unwrap();
        fs::write(&path, original).unwrap();
        let pending = store.begin(identity(), before, 100).unwrap();
        let durable =
            decode_manifest(&fs::read(path).unwrap(), store.limits.maximum_entries).unwrap();
        assert_eq!(durable.entries[&id].state, SnapshotState::Uncertain);
        assert_eq!(durable.entries[&id].reserved_bytes, 777);
        assert_eq!(store.inspect().unwrap().charged_bytes, 877);
        drop(pending);
    }

    #[test]
    fn definite_manifest_limit_does_not_create_an_uncertain_reservation() {
        let (_directory, mut store) = fixture();
        let path = store.lease.root().join(MANIFEST);
        let original = fs::read(&path).unwrap();
        Arc::get_mut(&mut store)
            .unwrap()
            .limits
            .maximum_manifest_bytes = original.len() as u64 + 8;
        assert!(matches!(
            store.begin(identity(), store.generation(), 1024),
            Err(CacheError::Limit)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(store.inspect().unwrap().charged_bytes, 0);
        assert!(store.runtime.lock().unwrap().manifest.entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn each_live_root_loss_refuses_cache_effects_preserving_conflicts_and_reservations() {
        for replaced in 0..3 {
            let (directory, store, roots) = scoped_fixture();
            let saved = saved(&store);
            let pending = store.begin(identity(), store.generation(), 1024).unwrap();
            fs::write(pending.path(), b"pending bytes remain").unwrap();
            let charged = store.inspect().unwrap().charged_bytes;
            let manifest_path = store.lease.root().join(MANIFEST);
            let manifest_bytes = fs::read(&manifest_path).unwrap();
            let retired = directory.path().join("retired");
            let replaced_root = roots[replaced].root();
            fs::rename(replaced_root, &retired).unwrap();
            fs::create_dir(replaced_root).unwrap();
            fs::write(replaced_root.join("foreign"), b"private untouched bytes").unwrap();
            assert!(matches!(
                store.begin(identity(), store.generation() + 1, 4),
                Err(CacheError::Conflict)
            ));
            assert_eq!(
                store
                    .begin(identity(), store.generation(), 4)
                    .err()
                    .unwrap()
                    .code(),
                "storage_root_unavailable"
            );
            assert!(matches!(
                store.publish(&pending, pending.revision() + 1),
                Err(CacheError::Conflict)
            ));
            assert_eq!(
                store
                    .publish(&pending, pending.revision())
                    .unwrap_err()
                    .code(),
                "storage_root_unavailable"
            );
            assert!(matches!(
                store.forget(&saved.id, &identity(), saved.revision + 1),
                Err(CacheError::Conflict)
            ));
            assert!(matches!(
                store.forget(&"f".repeat(32), &identity(), 1),
                Err(CacheError::NotFound)
            ));
            let mut foreign = identity();
            foreign.principal = "unrelated-principal".into();
            assert!(matches!(
                store.forget(&saved.id, &foreign, saved.revision),
                Err(CacheError::NotFound)
            ));
            assert_eq!(
                store
                    .forget(&saved.id, &identity(), saved.revision)
                    .unwrap_err()
                    .code(),
                "storage_root_unavailable"
            );
            assert_eq!(
                fs::read(replaced_root.join("foreign")).unwrap(),
                b"private untouched bytes"
            );
            assert_eq!(fs::read_dir(replaced_root).unwrap().count(), 1);
            drop(pending);
            fs::remove_dir_all(replaced_root).unwrap();
            fs::rename(&retired, replaced_root).unwrap();
            assert_eq!(fs::read(manifest_path).unwrap(), manifest_bytes);
            let inspection = store.inspect().unwrap();
            assert_eq!(inspection.charged_bytes, charged);
            assert!(
                inspection
                    .snapshots
                    .iter()
                    .any(|item| item.snapshot.state == SnapshotState::Uncertain)
            );
            assert_eq!(
                fs::read(store.path(&saved.id)).unwrap(),
                b"native KV fixture bytes"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn loss_between_intent_publication_and_creation_keeps_durable_charge_without_stage() {
        let (directory, store, roots) = scoped_fixture();
        let other = roots
            .iter()
            .find(|root| root.root().ends_with("artifacts"))
            .unwrap();
        let id = "3".repeat(32);
        let error = store
            .begin_observed(
                identity(),
                store.generation(),
                555,
                || Ok(id.clone()),
                |point, _| {
                    if point == BeginPoint::AfterCommit {
                        fs::rename(other.root(), directory.path().join("retired")).unwrap();
                        fs::create_dir(other.root()).unwrap();
                        fs::write(other.root().join("foreign"), b"untouched").unwrap();
                    }
                },
            )
            .err()
            .unwrap();
        assert_eq!(error.code(), "storage_root_unavailable");
        assert!(!store.path(&id).exists());
        assert_eq!(store.inspect().unwrap().charged_bytes, 555);
        assert_eq!(
            store
                .list(&identity().principal, &identity().credential_epoch)
                .unwrap()[0]
                .state,
            SnapshotState::Uncertain
        );
        let durable = decode_manifest(
            &fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            store.limits.maximum_entries,
        )
        .unwrap();
        assert_eq!(durable.entries[&id].reserved_bytes, 555);
        assert_eq!(durable.entries[&id].state, SnapshotState::Pending);
        assert_eq!(fs::read_dir(other.root()).unwrap().count(), 1);
        assert_eq!(
            fs::read(other.root().join("foreign")).unwrap(),
            b"untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn loss_before_initial_commit_is_typed_and_retains_conservative_memory_charge() {
        let (directory, store, roots) = scoped_fixture();
        let other = roots
            .iter()
            .find(|root| root.root().ends_with("artifacts"))
            .unwrap();
        let before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let id = "4".repeat(32);
        let error = store
            .begin_observed(
                identity(),
                store.generation(),
                444,
                || Ok(id.clone()),
                |point, _| {
                    if point == BeginPoint::BeforeCommit {
                        fs::rename(other.root(), directory.path().join("retired")).unwrap();
                        fs::create_dir(other.root()).unwrap();
                        fs::write(other.root().join("foreign"), b"untouched").unwrap();
                    }
                },
            )
            .err()
            .unwrap();
        assert_eq!(error.code(), "storage_root_unavailable");
        assert_eq!(fs::read(store.lease.root().join(MANIFEST)).unwrap(), before);
        assert_eq!(store.inspect().unwrap().charged_bytes, 444);
        assert_eq!(
            store
                .list(&identity().principal, &identity().credential_epoch)
                .unwrap()[0]
                .state,
            SnapshotState::Uncertain
        );
        assert!(!store.path(&id).exists());
        assert_eq!(fs::read_dir(other.root()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn scoped_open_rejects_sibling_loss_before_any_cache_manifest_is_created() {
        let directory = tempfile::tempdir().unwrap();
        let cache = DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let other = cache.claim(directory.path().join("other")).unwrap();
        let scope = LiveRoots::new(vec![cache.clone(), other.clone()]).unwrap();
        fs::rename(other.root(), directory.path().join("retired")).unwrap();
        fs::create_dir(other.root()).unwrap();
        assert_eq!(
            CacheStore::open_scoped(cache.clone(), CacheLimits::default(), scope)
                .err()
                .unwrap()
                .code(),
            "storage_root_unavailable"
        );
        assert!(!cache.root().join(MANIFEST).exists());
        assert_eq!(fs::read_dir(other.root()).unwrap().count(), 0);
    }

    #[test]
    fn publish_and_restart_preserve_validated_bytes_and_identity() {
        let (directory, store) = fixture();
        let entry = saved(&store);
        assert_eq!(entry.state, SnapshotState::Saved);
        let path = store.restore(&entry.id, &identity()).unwrap();
        assert_eq!(fs::read(path.path()).unwrap(), b"native KV fixture bytes");
        drop(path);
        drop(store);
        let store = reopened(directory.path());
        assert!(store.restore(&entry.id, &identity()).is_ok());
        assert_eq!(
            store.inspect().unwrap().snapshots[0].checksum_valid,
            Some(true)
        );
    }

    #[test]
    fn credentials_protocol_session_and_execution_never_cross_restore_namespaces() {
        let (_directory, store) = fixture();
        let entry = saved(&store);
        for field in 0..5 {
            let mut other = identity();
            match field {
                0 => other.principal = "configured-admin".into(),
                1 => other.credential_epoch = "d".repeat(64),
                2 => other.protocol = "anthropic".into(),
                3 => other.session_digest = "e".repeat(64),
                _ => other.execution_fingerprint = "f".repeat(64),
            }
            let error = store.restore(&entry.id, &other).err().unwrap();
            assert_eq!(
                error.code(),
                if field < 2 {
                    "snapshot_not_found"
                } else {
                    "snapshot_identity_mismatch"
                }
            );
        }
        assert!(
            store
                .list("configured-admin", &identity().credential_epoch)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn corruption_is_detected_without_mutating_or_discarding_the_snapshot() {
        let (_directory, store) = fixture();
        let entry = saved(&store);
        let path = store.path(&entry.id);
        fs::write(&path, b"modified KV fixture bytes").unwrap();
        let before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        assert_eq!(
            store.restore(&entry.id, &identity()).err().unwrap().code(),
            "snapshot_corrupt"
        );
        let inspection = store.inspect().unwrap();
        assert_eq!(inspection.snapshots[0].checksum_valid, Some(false));
        assert!(!inspection.recovery_authorized);
        assert_eq!(fs::read(store.lease.root().join(MANIFEST)).unwrap(), before);
        assert!(path.exists());
    }

    #[test]
    fn active_readers_and_pending_writers_block_delete_and_conflicts_preserve_state() {
        let (_directory, store) = fixture();
        let entry = saved(&store);
        let reader = store.restore(&entry.id, &identity()).unwrap();
        assert_eq!(
            store
                .forget(&entry.id, &identity(), entry.revision)
                .unwrap_err()
                .code(),
            "snapshot_busy"
        );
        drop(reader);
        assert_eq!(
            store
                .forget(&entry.id, &identity(), entry.revision - 1)
                .unwrap_err()
                .code(),
            "cache_revision_conflict"
        );
        let pending = store.begin(identity(), store.generation(), 1024).unwrap();
        assert_eq!(
            store
                .forget(pending.id(), &identity(), pending.revision())
                .unwrap_err()
                .code(),
            "snapshot_busy"
        );
        store
            .forget(&entry.id, &identity(), entry.revision)
            .unwrap();
        assert!(!store.path(&entry.id).exists());
        assert_eq!(
            store
                .list(&identity().principal, &identity().credential_epoch)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn abandoned_and_restarted_writes_stay_uncertain_and_charged() {
        let (directory, store) = fixture();
        let pending = store.begin(identity(), store.generation(), 1024).unwrap();
        let id = pending.id().to_owned();
        fs::write(pending.path(), b"incomplete bytes").unwrap();
        drop(pending);
        let before = store.inspect().unwrap();
        assert_eq!(before.charged_bytes, 1024);
        assert_eq!(before.snapshots[0].snapshot.state, SnapshotState::Uncertain);
        assert_eq!(
            store.forget(&id, &identity(), 1).unwrap_err().code(),
            "snapshot_uncertain"
        );
        drop(store);
        let reopened = reopened(directory.path());
        let after = reopened.inspect().unwrap();
        assert_eq!(after.charged_bytes, 1024);
        assert_eq!(after.snapshots[0].snapshot.state, SnapshotState::Uncertain);
        assert!(reopened.path(&id).exists());
    }

    #[test]
    fn capacity_and_generation_guards_prevent_unbounded_or_stale_writes() {
        let directory = tempfile::tempdir().unwrap();
        let store = CacheStore::open_with_limits(
            DirectoryLease::acquire(directory.path().join("cache")).unwrap(),
            CacheLimits {
                maximum_snapshot_bytes: 32,
                maximum_total_bytes: 32,
                ..CacheLimits::default()
            },
        )
        .unwrap();
        let pending = store.begin(identity(), 0, 32).unwrap();
        assert_eq!(
            store
                .begin(identity(), store.generation(), 1)
                .err()
                .unwrap()
                .code(),
            "cache_limit"
        );
        assert_eq!(
            store.begin(identity(), 0, 1).err().unwrap().code(),
            "cache_revision_conflict"
        );
        fs::write(pending.path(), [0u8; 33]).unwrap();
        assert_eq!(
            store.publish(&pending, 1).unwrap_err().code(),
            "cache_limit"
        );
        drop(pending);
        assert_eq!(store.inspect().unwrap().charged_bytes, 33);
    }

    #[test]
    fn empty_snapshots_cannot_be_published_as_valid_cache_state() {
        let (_directory, store) = fixture();
        let pending = store.begin(identity(), store.generation(), 1024).unwrap();
        assert_eq!(
            store
                .publish(&pending, pending.revision())
                .unwrap_err()
                .code(),
            "snapshot_corrupt"
        );
        drop(pending);
        assert_eq!(store.inspect().unwrap().charged_bytes, 1024);
    }

    #[test]
    fn orphan_bytes_remain_charged_and_manifest_size_limits_preserve_generation() {
        let directory = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let store = CacheStore::open_with_limits(
            lease,
            CacheLimits {
                maximum_total_bytes: 32,
                ..CacheLimits::default()
            },
        )
        .unwrap();
        let orphan = store.path(&"x".repeat(32));
        fs::write(&orphan, [0u8; 32]).unwrap();
        assert_eq!(store.inspect().unwrap().charged_bytes, 32);
        assert_eq!(store.inspect().unwrap().orphan_files.len(), 1);
        assert_eq!(
            store.begin(identity(), 0, 1).err().unwrap().code(),
            "cache_limit"
        );
        assert!(orphan.exists());
        let lease = store.lease.clone();
        drop(store);
        fs::remove_file(orphan).unwrap();
        let store = CacheStore::open_with_limits(
            lease,
            CacheLimits {
                maximum_manifest_bytes: 128,
                ..CacheLimits::default()
            },
        )
        .unwrap();
        assert_eq!(
            store.begin(identity(), 0, 1).err().unwrap().code(),
            "cache_limit"
        );
        assert_eq!(store.generation(), 0);
        assert!(
            store
                .list(&identity().principal, &identity().credential_epoch)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_identity_traversal_and_unrecognized_manifests_are_rejected() {
        let (_directory, store) = fixture();
        for id in ["../../secret", "a/b", "", "kv-name.bin"] {
            assert_eq!(
                store.restore(id, &identity()).err().unwrap().code(),
                "cache_invalid"
            );
        }
        let mut invalid = identity();
        invalid.session_digest = "raw session".into();
        assert_eq!(
            store
                .begin(invalid, store.generation(), 1024)
                .err()
                .unwrap()
                .code(),
            "cache_invalid"
        );
        let manifest = store.lease.root().join(MANIFEST);
        let held_root = store.lease.clone();
        drop(store);
        fs::write(
            &manifest,
            json!({"schema_version":9,"generation":0,"entries":{}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            CacheStore::open(held_root).err().unwrap().code(),
            "snapshot_corrupt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_and_manifest_symlinks_do_not_read_external_bytes() {
        use std::os::unix::fs::symlink;
        let (directory, store) = fixture();
        let entry = saved(&store);
        let external = directory.path().join("external");
        fs::write(&external, b"external private bytes").unwrap();
        fs::remove_file(store.path(&entry.id)).unwrap();
        symlink(&external, store.path(&entry.id)).unwrap();
        assert_eq!(
            store.restore(&entry.id, &identity()).err().unwrap().code(),
            "snapshot_corrupt"
        );
        assert_eq!(fs::read(&external).unwrap(), b"external private bytes");
        let manifest = store.lease.root().join(MANIFEST);
        let held_root = store.lease.clone();
        drop(store);
        fs::remove_file(&manifest).unwrap();
        symlink(&external, &manifest).unwrap();
        assert_eq!(
            CacheStore::open(held_root).err().unwrap().code(),
            "snapshot_corrupt"
        );
    }

    #[test]
    fn inspection_limits_leave_validation_unknown_and_do_not_grant_recovery() {
        let (_directory, store) = fixture();
        let entry = saved(&store);
        let lease = store.lease.clone();
        drop(store);
        let store = CacheStore::open_with_limits(
            lease,
            CacheLimits {
                maximum_hash_bytes: 1,
                ..CacheLimits::default()
            },
        )
        .unwrap();
        let inspection = store.inspect().unwrap();
        assert_eq!(inspection.snapshots[0].snapshot.id, entry.id);
        assert_eq!(inspection.snapshots[0].checksum_valid, None);
        assert!(inspection.snapshots[0].issues.contains(&"inspection_limit"));
        assert!(!inspection.recovery_authorized);
    }
}
