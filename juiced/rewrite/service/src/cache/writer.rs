use super::*;
use crate::writer_identity::WriterIdentity;

#[derive(Clone)]
pub struct OperationControl {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl OperationControl {
    pub fn new(deadline: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            deadline,
            cancelled,
        }
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn cancelled(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
    pub fn check(&self) -> Result<(), CacheError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(CacheError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(CacheError::Deadline);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct WriterRequest {
    pub model: String,
    pub generation: u64,
    pub actor: Option<WriterIdentity>,
}

impl WriterRequest {
    pub(super) fn validate(&self) -> Result<(), CacheError> {
        if self.model.is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
            || self.generation == 0
        {
            return Err(CacheError::Invalid(
                "Writer requires an exact model and positive owned generation",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActorBinding {
    pub(super) schema_version: u32,
    pub(super) store_id: String,
    pub(super) manager_owner: String,
    pub(super) model: String,
    pub(super) generation: u64,
    pub(super) write_key: String,
    pub(super) actor: Option<WriterIdentity>,
    pub(super) binding_digest: String,
    pub(super) cleanup_eligible: bool,
}

pub(super) type WriterBinding = ActorBinding;

impl std::fmt::Debug for ActorBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterBinding")
            .field("schema_version", &self.schema_version)
            .field("actor_present", &self.actor.is_some())
            .field("cleanup_eligible", &self.cleanup_eligible)
            .finish_non_exhaustive()
    }
}

pub(super) fn opaque_valid(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl ActorBinding {
    pub(super) fn new(
        store_id: &str,
        owner: &str,
        id: &str,
        identity: &SnapshotIdentity,
        reserved: u64,
        request: WriterRequest,
    ) -> Result<Self, CacheError> {
        let mut binding = Self::authority(store_id, owner, request)?;
        binding.binding_digest = binding.digest(id, identity, reserved)?;
        Ok(binding)
    }

    pub(super) fn authority(
        store_id: &str,
        owner: &str,
        request: WriterRequest,
    ) -> Result<Self, CacheError> {
        request.validate()?;
        Ok(Self {
            schema_version: 1,
            store_id: store_id.into(),
            manager_owner: owner.into(),
            model: request.model,
            generation: request.generation,
            write_key: crate::storage::generated_id()?,
            actor: request.actor,
            binding_digest: String::new(),
            cleanup_eligible: false,
        })
    }

    pub(super) fn validate_authority(&self, store_id: Option<&str>) -> Result<(), CacheError> {
        if self.schema_version != 1
            || Some(self.store_id.as_str()) != store_id
            || !opaque_valid(&self.store_id)
            || !opaque_valid(&self.manager_owner)
            || !opaque_valid(&self.write_key)
            || self.model.is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
            || self.generation == 0
        {
            return Err(CacheError::Corrupt("Cache actor authority is invalid"));
        }
        Ok(())
    }

    pub(super) fn matches(&self, owner: &str, request: &WriterRequest) -> bool {
        self.current_owner(owner)
            && self.model == request.model
            && self.generation == request.generation
            && self.actor == request.actor
    }

    pub(super) fn exited(
        &self,
        store: &CacheStore,
        owner: &str,
        control: &OperationControl,
    ) -> Result<bool, CacheError> {
        control.check()?;
        if self.current_owner(owner) {
            Ok(store
                .leases
                .lock()
                .map_err(|_| CacheError::Uncertain)?
                .exit_receipts
                .get(&self.model)
                .and_then(|generations| generations.get(&self.generation))
                .copied()
                .unwrap_or(false))
        } else {
            Ok(self.actor.as_ref().is_some_and(|actor| {
                crate::writer_identity::observe(actor, control.deadline(), || {
                    control.cancelled().load(Ordering::Acquire)
                })
                .state
                    == crate::writer_identity::State::Exited
            }))
        }
    }

    fn digest(
        &self,
        id: &str,
        identity: &SnapshotIdentity,
        reserved: u64,
    ) -> Result<String, CacheError> {
        binding_digest(&(
            "juiced-cache-writer-v1",
            self.schema_version,
            &self.store_id,
            &self.manager_owner,
            &self.model,
            self.generation,
            &self.write_key,
            id,
            identity,
            reserved,
            &self.actor,
        ))
    }

    pub(super) fn reader_digest(
        &self,
        reader: &str,
        snapshot: &str,
        identity: &SnapshotIdentity,
        bytes: u64,
        sha256: &str,
    ) -> Result<String, CacheError> {
        binding_digest(&(
            "juiced-cache-reader-v1",
            self.schema_version,
            &self.store_id,
            &self.manager_owner,
            &self.model,
            self.generation,
            &self.write_key,
            reader,
            snapshot,
            identity,
            bytes,
            sha256,
            &self.actor,
        ))
    }

    pub(super) fn validate(
        &self,
        store_id: Option<&str>,
        id: &str,
        record: &Record,
    ) -> Result<(), CacheError> {
        self.validate_authority(store_id)?;
        if self.binding_digest != self.digest(id, &record.identity, record.reserved_bytes)?
            || (self.cleanup_eligible && record.state != SnapshotState::Deleting)
        {
            return Err(CacheError::Corrupt(
                "Writer binding does not match its immutable reservation",
            ));
        }
        Ok(())
    }

    pub(super) fn register_receipt(&self, leases: &mut LeaseRegistry) {
        leases
            .exit_receipts
            .entry(self.model.clone())
            .or_default()
            .entry(self.generation)
            .or_insert(false);
    }

    pub(super) fn current_owner(&self, owner: &str) -> bool {
        self.manager_owner == owner
    }
    pub(super) fn actor_present(&self) -> bool {
        self.actor.is_some()
    }
    pub(super) fn cleanup_eligible(&self) -> bool {
        self.cleanup_eligible
    }
}

pub(super) fn binding_digest<T: Serialize + ?Sized>(value: &T) -> Result<String, CacheError> {
    struct HashSink(Sha256);
    impl Write for HashSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut sink = HashSink(Sha256::new());
    serde_json::to_writer(&mut sink, value)
        .map_err(|_| CacheError::Corrupt("Cache actor binding cannot be encoded"))?;
    Ok(format!("{:x}", sink.0.finalize()))
}

pub(super) fn hash_budget(limits: &CacheLimits, control: Option<&OperationControl>) -> HashBudget {
    let mut budget = HashBudget::new(limits);
    if let Some(control) = control {
        budget.deadline = budget.deadline.min(control.deadline);
        budget.cancelled = Some(control.cancelled.clone());
    }
    budget
}

pub(super) fn controlled_error(
    error: CacheError,
    budget: &HashBudget,
    control: Option<&OperationControl>,
) -> CacheError {
    if matches!(error, CacheError::Limit)
        && let Some(control) = control
    {
        if let Err(error) = control.check() {
            return error;
        }
        if Instant::now() >= budget.deadline {
            return CacheError::Deadline;
        }
    }
    error
}

#[derive(Debug, Serialize)]
pub struct RecoveryOutcome {
    pub recovered: bool,
    pub snapshot: String,
    pub generation: u64,
    pub journal_cleanup_pending: bool,
}

impl CacheStore {
    pub fn begin_bound(
        self: &Arc<Self>,
        identity: SnapshotIdentity,
        expected_generation: u64,
        maximum_bytes: u64,
        writer: WriterRequest,
        control: &OperationControl,
    ) -> Result<PendingSnapshot, CacheError> {
        self.begin_impl(
            identity,
            expected_generation,
            maximum_bytes,
            Some((writer, control)),
            crate::storage::generated_id,
            |_, _| {},
        )
    }

    pub fn validate_writer(
        &self,
        pending: &PendingSnapshot,
        writer: &WriterRequest,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        writer.validate()?;
        if !std::ptr::eq(self, pending.store.as_ref()) {
            return Err(CacheError::NotFound);
        }
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        let record = authorized(&runtime.manifest, &pending.id, &pending.identity)?;
        if record.revision != pending.revision {
            return Err(CacheError::Conflict);
        }
        let bound = record.writer.as_ref().ok_or(CacheError::Uncertain)?;
        bound.validate(runtime.manifest.store_id.as_deref(), &pending.id, record)?;
        if record.state != SnapshotState::Pending
            || !runtime.writing(&pending.id)
            || !bound.current_owner(&runtime.manager_owner)
            || bound.model != writer.model
            || bound.generation != writer.generation
            || bound.actor != writer.actor
            || bound.cleanup_eligible
        {
            return Err(CacheError::Uncertain);
        }
        drop(runtime);
        self.verify_scope()?;
        control.check()
    }

    pub(crate) fn acknowledge_writer_exit(&self, model: &str, generation: u64) {
        if let Ok(mut leases) = self.leases.lock()
            && let Some(receipt) = leases
                .exit_receipts
                .get_mut(model)
                .and_then(|generations| generations.get_mut(&generation))
        {
            *receipt = true;
        }
    }

    pub(super) fn prune_exit_receipt(&self, runtime: &Runtime, binding: &WriterBinding) {
        let needed = runtime.manifest.entries.values().any(|record| {
            record.state != SnapshotState::Saved
                && record.writer.as_ref().is_some_and(|writer| {
                    writer.current_owner(&runtime.manager_owner)
                        && writer.model == binding.model
                        && writer.generation == binding.generation
                })
        });
        let needed = needed
            || runtime.manifest.reader_intents.values().any(|reader| {
                reader.binding.current_owner(&runtime.manager_owner)
                    && reader.binding.model == binding.model
                    && reader.binding.generation == binding.generation
            });
        if needed {
            return;
        }
        let mut leases = self
            .leases
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(generations) = leases.exit_receipts.get_mut(&binding.model) {
            generations.remove(&binding.generation);
            if generations.is_empty() {
                leases.exit_receipts.remove(&binding.model);
            }
        }
    }

    pub(super) fn prune_published_receipt(&self, runtime: &Runtime, id: &str) {
        if let Some(binding) = runtime
            .manifest
            .entries
            .get(id)
            .and_then(|record| record.writer.as_ref())
        {
            self.prune_exit_receipt(runtime, binding);
        }
    }

    pub fn recover_writer(
        &self,
        id: &str,
        identity: &SnapshotIdentity,
        expected_revision: u64,
        control: &OperationControl,
    ) -> Result<RecoveryOutcome, CacheError> {
        self.recover_observed(
            id,
            identity,
            expected_revision,
            control,
            |actor| {
                crate::writer_identity::observe(actor, control.deadline, || {
                    control.cancelled.load(Ordering::Acquire)
                })
                .state
                    == crate::writer_identity::State::Exited
            },
            |_| Ok(()),
        )
    }

    fn recover_observed(
        &self,
        id: &str,
        identity: &SnapshotIdentity,
        expected_revision: u64,
        control: &OperationControl,
        observe_exited: impl FnOnce(&WriterIdentity) -> bool,
        mut checkpoint: impl FnMut(RecoveryPoint) -> Result<(), CacheError>,
    ) -> Result<RecoveryOutcome, CacheError> {
        validate_id(id)?;
        identity.validate()?;
        control.check()?;
        let (writer, acknowledged) = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            let record = authorized(&runtime.manifest, id, identity)?;
            if record.revision != expected_revision {
                return Err(CacheError::Conflict);
            }
            if runtime.writing(id) || runtime.reader_count(id) > 0 {
                return Err(CacheError::Busy);
            }
            if record.state == SnapshotState::Saved {
                return Err(CacheError::Invalid(
                    "Saved snapshots use ordinary forgetting",
                ));
            }
            let writer = record.writer.clone().ok_or(CacheError::Uncertain)?;
            writer.validate(runtime.manifest.store_id.as_deref(), id, record)?;
            let acknowledged = writer.current_owner(&runtime.manager_owner)
                && self
                    .leases
                    .lock()
                    .map_err(|_| CacheError::Uncertain)?
                    .exit_receipts
                    .get(&writer.model)
                    .and_then(|generations| generations.get(&writer.generation))
                    .copied()
                    .unwrap_or(false);
            (writer, acknowledged)
        };
        let observed = if writer.cleanup_eligible || acknowledged {
            true
        } else {
            let current_owner = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?
                .manager_owner
                == writer.manager_owner;
            !current_owner && writer.actor.as_ref().is_some_and(observe_exited)
        };
        control.check()?;
        if !observed {
            return Err(CacheError::Uncertain);
        }
        self.verify_scope()?;
        {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            let record = authorized(&runtime.manifest, id, identity)?;
            if record.revision != expected_revision {
                return Err(CacheError::Conflict);
            }
            if runtime.writing(id) || runtime.reader_count(id) > 0 {
                return Err(CacheError::Busy);
            }
            if record.state == SnapshotState::Saved {
                return Err(CacheError::Conflict);
            }
            if !record
                .writer
                .as_ref()
                .is_some_and(|current| current.binding_digest == writer.binding_digest)
            {
                return Err(CacheError::Uncertain);
            }
            if !writer.cleanup_eligible {
                let mut next = runtime.manifest.clone();
                let record = Arc::make_mut(next.entries.get_mut(id).expect("authorized writer"));
                record.revision = record.revision.checked_add(1).ok_or(CacheError::Limit)?;
                record.state = SnapshotState::Deleting;
                record
                    .writer
                    .as_mut()
                    .expect("bound writer")
                    .cleanup_eligible = true;
                control.check()?;
                self.commit(&mut runtime, next)?;
            }
        }
        let retained = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
            authorized(&runtime.manifest, id, identity)?.clone()
        };
        let journal = RecoveryJournal::new(&self.store_id, id, retained.clone());
        self.ensure_recovery_journal(&journal, control)?;
        checkpoint(RecoveryPoint::BeforeUnlink)?;
        control.check()?;
        self.verify_scope()?;
        let path = self.path(id);
        match regular_file(&path) {
            Ok(file) => {
                self.verify_scope()?;
                control.check()?;
                if !same_file(&file, &path)? {
                    return Err(CacheError::Uncertain);
                }
                fs::remove_file(&path)?;
            }
            Err(CacheError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.verify_scope()?;
        self.sync_recovery_directory(control)?;
        self.verify_scope()?;
        control.check()?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        let record = authorized(&runtime.manifest, id, identity)?;
        if !record
            .writer
            .as_ref()
            .is_some_and(|writer| writer.cleanup_eligible)
            || runtime.writing(id)
            || runtime.reader_count(id) > 0
        {
            return Err(CacheError::Uncertain);
        }
        let mut next = runtime.manifest.clone();
        next.entries.remove(id);
        checkpoint(RecoveryPoint::BeforeManifestCommit)?;
        if let Err(error) = self
            .commit(&mut runtime, next)
            .and_then(|_| checkpoint(RecoveryPoint::AfterManifestCommit))
        {
            runtime
                .manifest
                .entries
                .entry(id.to_owned())
                .or_insert_with(|| Arc::new(retained));
            return Err(error);
        }
        drop(runtime);
        let journal_cleanup_pending = checkpoint(RecoveryPoint::BeforeJournalCleanup)
            .and_then(|_| self.remove_recovery_journal(&journal, control))
            .is_err();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| CacheError::Corrupt("Cache ownership state is unavailable"))?;
        if journal_cleanup_pending {
            runtime
                .manifest
                .entries
                .entry(id.to_owned())
                .or_insert_with(|| Arc::new(journal.record.clone()));
        }
        self.prune_exit_receipt(&runtime, &writer);
        Ok(RecoveryOutcome {
            recovered: true,
            snapshot: id.to_owned(),
            generation: runtime.manifest.generation,
            journal_cleanup_pending,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryPoint {
    BeforeUnlink,
    BeforeManifestCommit,
    AfterManifestCommit,
    BeforeJournalCleanup,
}

const JOURNAL_PREFIX: &str = ".recovery-";
const MAXIMUM_JOURNAL_BYTES: u64 = 8192;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryJournal {
    schema_version: u32,
    store_id: String,
    snapshot_id: String,
    record: Record,
}

impl RecoveryJournal {
    fn new(store_id: &str, snapshot_id: &str, record: Record) -> Self {
        Self {
            schema_version: 1,
            store_id: store_id.into(),
            snapshot_id: snapshot_id.into(),
            record,
        }
    }

    fn filename(&self) -> Result<String, CacheError> {
        let writer = self.record.writer.as_ref().ok_or(CacheError::Uncertain)?;
        Ok(format!(
            "{JOURNAL_PREFIX}{}-{}.json",
            self.snapshot_id, writer.write_key
        ))
    }

    fn validate(
        &self,
        filename: &str,
        store_id: Option<&str>,
        maximum: u64,
    ) -> Result<(), CacheError> {
        validate_id(&self.snapshot_id)?;
        let writer = self.record.writer.as_ref().ok_or(CacheError::Uncertain)?;
        validate_record(store_id, &self.snapshot_id, &self.record, maximum)?;
        if self.schema_version != 1
            || Some(self.store_id.as_str()) != store_id
            || self.filename()? != filename
            || self.record.state != SnapshotState::Deleting
            || !writer.cleanup_eligible
            || self.record.reserved_bytes == 0
            || self.record.reserved_bytes > maximum
            || self.record.revision == 0
        {
            return Err(CacheError::Corrupt(
                "Recovery journal does not match its retained reservation",
            ));
        }
        Ok(())
    }
}

pub(super) fn recovery_journal_name(name: &str) -> bool {
    name.starts_with(JOURNAL_PREFIX) || reader::reader_journal_name(name)
}

pub(super) fn decode_recovery_journal(
    bytes: &[u8],
    filename: &str,
    manifest: &mut Manifest,
    maximum_entries: usize,
    maximum_snapshot_bytes: u64,
) -> Result<(), CacheError> {
    if reader::reader_journal_name(filename) {
        return reader::decode_reader_journal(
            bytes,
            filename,
            manifest,
            maximum_entries,
            maximum_snapshot_bytes,
        );
    }
    if bytes.len() as u64 > MAXIMUM_JOURNAL_BYTES || !journal_name_valid(filename) {
        return Err(CacheError::Limit);
    }
    let journal: RecoveryJournal = serde_json::from_slice(bytes)
        .map_err(|_| CacheError::Corrupt("Recovery journal is unreadable"))?;
    journal.validate(
        filename,
        manifest.store_id.as_deref(),
        maximum_snapshot_bytes,
    )?;
    match manifest.entries.get(&journal.snapshot_id) {
        Some(record) if record.as_ref() != &journal.record => Err(CacheError::Corrupt(
            "Recovery journal conflicts with cache metadata",
        )),
        Some(_) => Ok(()),
        None => {
            if manifest
                .entries
                .len()
                .checked_add(manifest.reader_intents.len())
                .is_none_or(|count| count >= maximum_entries)
            {
                return Err(CacheError::Limit);
            }
            manifest
                .entries
                .insert(journal.snapshot_id, Arc::new(journal.record));
            Ok(())
        }
    }
}

pub(super) const fn maximum_recovery_journal_bytes() -> u64 {
    MAXIMUM_JOURNAL_BYTES
}

pub(super) fn journal_name_valid(name: &str) -> bool {
    if reader::reader_journal_name(name) {
        return reader::reader_journal_name_valid(name);
    }
    name.strip_prefix(JOURNAL_PREFIX)
        .and_then(|name| name.strip_suffix(".json"))
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(id, key)| validate_id(id).is_ok() && opaque_valid(key))
}

pub(super) fn recovery_journal_usage(
    lease: &DirectoryLease,
    limits: &CacheLimits,
) -> Result<(usize, u64), CacheError> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for (index, entry) in fs::read_dir(lease.root())?.enumerate() {
        if index > limits.maximum_entries.saturating_mul(3).saturating_add(16) {
            return Err(CacheError::Limit);
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| recovery_journal_name(name)) else {
            continue;
        };
        if !journal_name_valid(name) {
            return Err(CacheError::Corrupt("Recovery journal name is invalid"));
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAXIMUM_JOURNAL_BYTES
        {
            return Err(CacheError::Corrupt(
                "Recovery journal is not bounded regular metadata",
            ));
        }
        count += 1;
        bytes = bytes.checked_add(metadata.len()).ok_or(CacheError::Limit)?;
        if count > limits.maximum_entries || bytes > limits.maximum_manifest_bytes {
            return Err(CacheError::Limit);
        }
    }
    Ok((count, bytes))
}

pub(super) fn refuse_missing_manifest_journals(
    lease: &DirectoryLease,
    limits: &CacheLimits,
) -> Result<(), CacheError> {
    for (index, entry) in fs::read_dir(lease.root())?.enumerate() {
        if index > limits.maximum_entries.saturating_mul(3).saturating_add(16) {
            return Err(CacheError::Limit);
        }
        if entry?
            .file_name()
            .to_str()
            .is_some_and(recovery_journal_name)
        {
            return Err(CacheError::Corrupt(
                "Recovery journals require their original cache manifest",
            ));
        }
    }
    Ok(())
}

pub(super) fn load_recovery_journals(
    lease: &DirectoryLease,
    scope: &LiveRoots,
    limits: &CacheLimits,
    manifest: &mut Manifest,
) -> Result<(), CacheError> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for (index, entry) in fs::read_dir(lease.root())?.enumerate() {
        if index > limits.maximum_entries.saturating_mul(3).saturating_add(16) {
            return Err(CacheError::Limit);
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| recovery_journal_name(name)) else {
            continue;
        };
        if !journal_name_valid(name) {
            return Err(CacheError::Corrupt("Recovery journal name is invalid"));
        }
        count += 1;
        if count > limits.maximum_entries {
            return Err(CacheError::Limit);
        }
        scope.verify().map_err(|_| CacheError::RootUnavailable)?;
        let content = lease.read_bytes(name, MAXIMUM_JOURNAL_BYTES)?;
        bytes = bytes
            .checked_add(content.len() as u64)
            .ok_or(CacheError::Limit)?;
        if bytes > limits.maximum_manifest_bytes {
            return Err(CacheError::Limit);
        }
        decode_recovery_journal(
            &content,
            name,
            manifest,
            limits.maximum_entries,
            limits.maximum_snapshot_bytes,
        )?;
    }
    scope.verify().map_err(|_| CacheError::RootUnavailable)?;
    Ok(())
}

impl CacheStore {
    fn ensure_recovery_journal(
        &self,
        journal: &RecoveryJournal,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        self.verify_scope()?;
        let filename = journal.filename()?;
        match self.lease.read_bytes(&filename, MAXIMUM_JOURNAL_BYTES) {
            Ok(content) => {
                let previous: RecoveryJournal = serde_json::from_slice(&content)
                    .map_err(|_| CacheError::Corrupt("Recovery journal is unreadable"))?;
                previous.validate(
                    &filename,
                    Some(&self.store_id),
                    self.limits.maximum_snapshot_bytes,
                )?;
                if previous.record != journal.record {
                    return Err(CacheError::Corrupt(
                        "Recovery journal conflicts with its writer",
                    ));
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let content = serde_json::to_vec(journal)
                    .map_err(|_| CacheError::Corrupt("Recovery journal cannot be encoded"))?;
                if content.len() as u64 > MAXIMUM_JOURNAL_BYTES
                    || content.len() as u64 > self.limits.maximum_manifest_bytes
                {
                    return Err(CacheError::Limit);
                }
                let (count, total) = recovery_journal_usage(&self.lease, &self.limits)?;
                if count >= self.limits.maximum_entries
                    || total
                        .checked_add(content.len() as u64 + 1)
                        .is_none_or(|bytes| bytes > self.limits.maximum_manifest_bytes)
                {
                    return Err(CacheError::Limit);
                }
                control.check()?;
                self.scope
                    .write_json(&self.lease, &filename, journal)
                    .map_err(CacheError::Persistence)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn sync_recovery_directory(
        &self,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        self.verify_scope()?;
        #[cfg(unix)]
        {
            let directory = File::open(self.lease.root())?;
            if !same_file(&directory, self.lease.root())? {
                return Err(CacheError::RootUnavailable);
            }
            self.verify_scope()?;
            directory.sync_all()?;
        }
        Ok(())
    }

    fn remove_recovery_journal(
        &self,
        journal: &RecoveryJournal,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        self.verify_scope()?;
        control.check()?;
        let filename = journal.filename()?;
        let content = self.lease.read_bytes(&filename, MAXIMUM_JOURNAL_BYTES)?;
        let previous: RecoveryJournal = serde_json::from_slice(&content)
            .map_err(|_| CacheError::Corrupt("Recovery journal is unreadable"))?;
        previous.validate(
            &filename,
            Some(&self.store_id),
            self.limits.maximum_snapshot_bytes,
        )?;
        if previous.record != journal.record {
            return Err(CacheError::Uncertain);
        }
        let path = self.lease.file_path(filename)?;
        let file = regular_file(&path)?;
        self.verify_scope()?;
        if !same_file(&file, &path)? {
            return Err(CacheError::Uncertain);
        }
        control.check()?;
        fs::remove_file(path)?;
        self.sync_recovery_directory(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{fixture, identity};
    use serde_json::json;

    fn control() -> OperationControl {
        OperationControl::new(
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        )
    }
    fn request() -> WriterRequest {
        WriterRequest {
            model: "model-one".into(),
            generation: 7,
            actor: None,
        }
    }
    fn bound(store: &Arc<CacheStore>) -> PendingSnapshot {
        store
            .begin_bound(identity(), store.generation(), 1024, request(), &control())
            .unwrap()
    }
    fn charge(store: &CacheStore) -> u64 {
        store
            .charged(&store.runtime.lock().unwrap().manifest)
            .unwrap()
    }
    fn actor() -> WriterIdentity {
        serde_json::from_value(json!({"version":1,"method":"future-observe-only","pid":42,"domain":"a".repeat(64),"token":"b".repeat(64)})).unwrap()
    }
    fn reopen(store: &Arc<CacheStore>) -> Arc<CacheStore> {
        CacheStore::open(store.lease.clone()).unwrap()
    }

    #[test]
    fn bound_intent_is_durable_before_stage_creation_and_binds_every_immutable_field() {
        let (_directory, store) = fixture();
        let id = "1".repeat(32);
        let pending = store
            .begin_impl(
                identity(),
                store.generation(),
                1024,
                Some((request(), &control())),
                || Ok(id.clone()),
                |point, key| {
                    if point == BeginPoint::AfterCommit {
                        assert!(!store.path(key).exists());
                        let content = fs::read(store.lease.root().join(MANIFEST)).unwrap();
                        let manifest = decode_manifest(&content, 100).unwrap();
                        validate_manifest(&manifest, 100, 2048).unwrap();
                        let record = &manifest.entries[key];
                        assert!(record.writer.is_some());
                        assert_eq!(record.reserved_bytes, 1024);
                        assert_eq!(record.state, SnapshotState::Pending);
                    }
                },
            )
            .unwrap();
        store
            .validate_writer(&pending, &request(), &control())
            .unwrap();
        let runtime = store.runtime.lock().unwrap();
        let original = runtime.manifest.clone();
        drop(runtime);
        for field in [
            "store_id",
            "manager_owner",
            "model",
            "generation",
            "write_key",
            "actor",
            "binding_digest",
        ] {
            let mut value = serde_json::to_value(&original).unwrap();
            let writer = &mut value["entries"][&id]["writer"];
            writer[field] = match field {
                "generation" => json!(8),
                "actor" => serde_json::to_value(actor()).unwrap(),
                "binding_digest" => json!("0".repeat(64)),
                _ => json!("2".repeat(32)),
            };
            let bytes = serde_json::to_vec(&value).unwrap();
            let decoded = decode_manifest(&bytes, 100).unwrap();
            assert!(
                validate_manifest(&decoded, 100, 2048).is_err(),
                "field={field}"
            );
        }
        let mut changed = original.clone();
        Arc::make_mut(changed.entries.get_mut(&id).unwrap()).reserved_bytes = 512;
        assert!(validate_manifest(&changed, 100, 2048).is_err());
        let mut changed = original;
        Arc::make_mut(changed.entries.get_mut(&id).unwrap())
            .identity
            .session_digest = "d".repeat(64);
        assert!(validate_manifest(&changed, 100, 2048).is_err());
        assert_eq!(charge(&store), 1024);
    }

    #[test]
    fn writer_revalidation_rejects_other_generation_store_and_request_without_side_effects() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let mut other = request();
        other.generation += 1;
        assert!(matches!(
            store.validate_writer(&pending, &other, &control()),
            Err(CacheError::Uncertain)
        ));
        other = request();
        other.actor = Some(actor());
        assert!(matches!(
            store.validate_writer(&pending, &other, &control()),
            Err(CacheError::Uncertain)
        ));
        let (_other_directory, other_store) = fixture();
        assert!(matches!(
            other_store.validate_writer(&pending, &request(), &control()),
            Err(CacheError::NotFound)
        ));
        assert_eq!(fs::read(store.lease.root().join(MANIFEST)).unwrap(), before);
    }

    #[test]
    fn legacy_unbound_and_prior_actorless_intents_never_gain_cleanup_authority() {
        let (_directory, store) = fixture();
        let legacy = store.begin(identity(), store.generation(), 1024).unwrap();
        let legacy_id = legacy.id().to_owned();
        drop(legacy);
        store.acknowledge_writer_exit("model-one", 7);
        assert!(matches!(
            store.recover_writer(&legacy_id, &identity(), 1, &control()),
            Err(CacheError::Uncertain)
        ));
        let pending = bound(&store);
        let id = pending.id().to_owned();
        drop(pending);
        let restarted = reopen(&store);
        restarted.acknowledge_writer_exit("model-one", 7);
        assert!(matches!(
            restarted.recover_writer(&id, &identity(), 1, &control()),
            Err(CacheError::Uncertain)
        ));
        assert_eq!(charge(&restarted), 2048);
        assert!(store.path(&legacy_id).exists());
        assert!(store.path(&id).exists());
    }

    #[test]
    fn exact_current_exit_acknowledgement_is_required_and_active_handles_remain_busy() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        fs::write(pending.path(), b"partial KV").unwrap();
        store.acknowledge_writer_exit("model-one", 7);
        assert!(matches!(
            store.recover_writer(&id, &identity(), 1, &control()),
            Err(CacheError::Busy)
        ));
        drop(pending);
        let result = store
            .recover_observed(
                &id,
                &identity(),
                1,
                &control(),
                |_| panic!("current ACK must not query actors"),
                |point| {
                    if point == RecoveryPoint::BeforeUnlink {
                        assert!(store.path(&id).exists());
                        let manifest = decode_manifest(
                            &fs::read(store.lease.root().join(MANIFEST)).unwrap(),
                            100,
                        )
                        .unwrap();
                        assert!(
                            manifest.entries[&id]
                                .writer
                                .as_ref()
                                .unwrap()
                                .cleanup_eligible
                        );
                        let journals = fs::read_dir(store.lease.root())
                            .unwrap()
                            .filter_map(Result::ok)
                            .filter(|entry| {
                                entry
                                    .file_name()
                                    .to_string_lossy()
                                    .starts_with(JOURNAL_PREFIX)
                            })
                            .count();
                        assert_eq!(journals, 1);
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(result.recovered);
        assert!(!result.journal_cleanup_pending);
        assert!(!store.path(&id).exists());
        assert_eq!(charge(&store), 0);
        assert_eq!(charge(&reopen(&store)), 0);
    }

    #[test]
    fn wrong_exit_ack_and_other_principal_or_revision_do_not_touch_bytes() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        fs::write(pending.path(), b"retained").unwrap();
        drop(pending);
        store.acknowledge_writer_exit("model-two", 7);
        store.acknowledge_writer_exit("model-one", 8);
        assert!(matches!(
            store.recover_writer(&id, &identity(), 1, &control()),
            Err(CacheError::Uncertain)
        ));
        let mut stranger = identity();
        stranger.principal = "another-principal".into();
        assert!(matches!(
            store.recover_writer(&id, &stranger, 1, &control()),
            Err(CacheError::NotFound)
        ));
        assert!(matches!(
            store.recover_writer(&id, &identity(), 2, &control()),
            Err(CacheError::Conflict)
        ));
        assert_eq!(fs::read(store.path(&id)).unwrap(), b"retained");
        assert_eq!(charge(&store), 1024);
    }

    #[test]
    fn prior_actor_requires_positive_exited_observation_and_unknown_method_stays_charged() {
        let (_directory, store) = fixture();
        let mut writer = request();
        writer.actor = Some(actor());
        let pending = store
            .begin_bound(identity(), store.generation(), 1024, writer, &control())
            .unwrap();
        let id = pending.id().to_owned();
        drop(pending);
        let restarted = reopen(&store);
        assert!(matches!(
            restarted.recover_writer(&id, &identity(), 1, &control()),
            Err(CacheError::Uncertain)
        ));
        assert!(matches!(
            restarted.recover_observed(&id, &identity(), 1, &control(), |_| false, |_| Ok(())),
            Err(CacheError::Uncertain)
        ));
        assert!(
            restarted
                .recover_observed(&id, &identity(), 1, &control(), |_| true, |_| Ok(()))
                .unwrap()
                .recovered
        );
        assert_eq!(charge(&restarted), 0);
    }

    #[test]
    fn committed_final_failure_keeps_durable_journal_charge_across_reopen_and_retries() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        fs::write(pending.path(), b"partial").unwrap();
        drop(pending);
        store.acknowledge_writer_exit("model-one", 7);
        let error = store
            .recover_observed(
                &id,
                &identity(),
                1,
                &control(),
                |_| false,
                |point| {
                    if point == RecoveryPoint::AfterManifestCommit {
                        assert!(!store.path(&id).exists());
                        let disk = decode_manifest(
                            &fs::read(store.lease.root().join(MANIFEST)).unwrap(),
                            100,
                        )
                        .unwrap();
                        assert!(!disk.entries.contains_key(&id));
                        return Err(CacheError::Persistence(AtomicWriteError::test_committed(
                            crate::storage::WriteStage::SyncDirectory,
                            io::Error::other("controlled acknowledgement loss after actual commit"),
                        )));
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(matches!(error, CacheError::Persistence(error) if error.committed()));
        assert_eq!(charge(&store), 1024);
        let disk_before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let restarted = reopen(&store);
        assert_eq!(
            fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            disk_before
        );
        assert_eq!(charge(&restarted), 1024);
        let metadata = restarted.list("configured-api", &"b".repeat(64)).unwrap();
        assert!(metadata[0].cleanup_eligible);
        assert!(
            restarted
                .recover_writer(&id, &identity(), metadata[0].revision, &control())
                .unwrap()
                .recovered
        );
        assert_eq!(charge(&restarted), 0);
        assert_eq!(charge(&reopen(&restarted)), 0);
    }

    #[test]
    fn final_uncommitted_failure_and_housekeeping_warning_have_distinct_outcomes() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        drop(pending);
        store.acknowledge_writer_exit("model-one", 7);
        assert!(matches!(
            store.recover_observed(
                &id,
                &identity(),
                1,
                &control(),
                |_| false,
                |point| {
                    if point == RecoveryPoint::BeforeManifestCommit {
                        return Err(CacheError::Io(io::Error::other(
                            "controlled failure before commit",
                        )));
                    }
                    Ok(())
                }
            ),
            Err(CacheError::Io(_))
        ));
        assert_eq!(charge(&reopen(&store)), 1024);
        let revision = store.list("configured-api", &"b".repeat(64)).unwrap()[0].revision;
        let outcome = store
            .recover_observed(
                &id,
                &identity(),
                revision,
                &control(),
                |_| false,
                |point| {
                    if point == RecoveryPoint::BeforeJournalCleanup {
                        return Err(CacheError::Io(io::Error::other(
                            "controlled housekeeping failure",
                        )));
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(outcome.recovered);
        assert!(outcome.journal_cleanup_pending);
        assert_eq!(charge(&reopen(&store)), 1024);
    }

    #[test]
    fn cancellation_and_deadline_are_typed_and_do_not_reset_hash_deadlines() {
        let (_directory, store) = fixture();
        let cancelled = control();
        cancelled.cancelled.store(true, Ordering::Release);
        assert!(matches!(
            store.begin_bound(identity(), store.generation(), 1024, request(), &cancelled),
            Err(CacheError::Cancelled)
        ));
        let expired = OperationControl::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        assert!(matches!(
            store.begin_bound(identity(), store.generation(), 1024, request(), &expired),
            Err(CacheError::Deadline)
        ));
        let pending = bound(&store);
        fs::write(pending.path(), b"valid KV").unwrap();
        assert!(matches!(
            store.publish_controlled(&pending, 1, &expired),
            Err(CacheError::Deadline)
        ));
        let saved = store.publish_controlled(&pending, 1, &control()).unwrap();
        assert!(matches!(
            store.restore_controlled(&saved.id, &identity(), &cancelled),
            Err(CacheError::Cancelled)
        ));
        assert!(matches!(
            store.restore_controlled(&saved.id, &identity(), &expired),
            Err(CacheError::Deadline)
        ));
        let valid = control();
        let budget = hash_budget(&store.limits, Some(&valid));
        assert!(budget.deadline <= valid.deadline());
        valid.cancelled.store(true, Ordering::Release);
        assert!(matches!(
            controlled_error(CacheError::Limit, &budget, Some(&valid)),
            CacheError::Cancelled
        ));
        assert_eq!(
            store.list("configured-api", &"b".repeat(64)).unwrap()[0].active_readers,
            0
        );
    }

    #[test]
    #[cfg(unix)]
    fn malformed_journals_symlinks_and_missing_manifest_never_authorize_cleanup() {
        use std::os::unix::fs::symlink;
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        drop(pending);
        store.acknowledge_writer_exit("model-one", 7);
        assert!(
            store
                .recover_observed(
                    &id,
                    &identity(),
                    1,
                    &control(),
                    |_| false,
                    |point| if point == RecoveryPoint::BeforeUnlink {
                        Err(CacheError::Uncertain)
                    } else {
                        Ok(())
                    }
                )
                .is_err()
        );
        let journal_path = fs::read_dir(store.lease.root())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(JOURNAL_PREFIX)
            })
            .unwrap()
            .path();
        let original = fs::read(&journal_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["record"]["writer"]["generation"] = json!(999);
        fs::write(&journal_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            CacheStore::open(store.lease.clone()),
            Err(CacheError::Corrupt(_))
        ));
        fs::write(&journal_path, &original).unwrap();
        let external = store
            .lease
            .root()
            .parent()
            .unwrap()
            .join("external-journal");
        fs::write(&external, &original).unwrap();
        fs::remove_file(&journal_path).unwrap();
        symlink(&external, &journal_path).unwrap();
        assert!(CacheStore::open(store.lease.clone()).is_err());
        assert_eq!(fs::read(&external).unwrap(), original);
        fs::remove_file(&journal_path).unwrap();
        fs::write(&journal_path, &original).unwrap();
        fs::remove_file(store.lease.root().join(MANIFEST)).unwrap();
        assert!(CacheStore::open(store.lease.clone()).is_err());
        assert!(!store.lease.root().join(MANIFEST).exists());
        assert!(store.path(&id).exists());
    }

    #[test]
    fn exit_ack_and_lease_drops_do_not_wait_for_manifest_io_lock() {
        let (_directory, store) = fixture();
        let saved_pending = store.begin(identity(), store.generation(), 1024).unwrap();
        fs::write(saved_pending.path(), b"saved KV").unwrap();
        let saved = store.publish(&saved_pending, 1).unwrap();
        drop(saved_pending);
        let read = store.restore(&saved.id, &identity()).unwrap();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        let manifest_lock = store.runtime.lock().unwrap();
        let (finished, received) = std::sync::mpsc::channel();
        let owner = store.clone();
        let thread = std::thread::spawn(move || {
            owner.acknowledge_writer_exit("model-one", 7);
            drop(pending);
            drop(read);
            let _ = finished.send(());
        });
        let result = received.recv_timeout(Duration::from_secs(1));
        drop(manifest_lock);
        thread.join().unwrap();
        result.expect("lease teardown waited for the filesystem-bearing manifest mutex");
        let leases = store.leases.lock().unwrap();
        assert!(!leases.active_writes.contains(&id));
        assert!(!leases.readers.contains_key(&saved.id));
        assert!(leases.exit_receipts["model-one"][&7]);
    }

    #[test]
    fn offline_inspection_charges_journal_only_and_duplicate_rows_without_recovery_authority() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        drop(pending);
        store.acknowledge_writer_exit("model-one", 7);
        assert!(
            store
                .recover_observed(
                    &id,
                    &identity(),
                    1,
                    &control(),
                    |_| false,
                    |point| {
                        if point == RecoveryPoint::BeforeUnlink {
                            return Err(CacheError::Uncertain);
                        }
                        Ok(())
                    }
                )
                .is_err()
        );
        let before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let duplicate =
            inspect_snapshot_store(store.lease.root(), &SnapshotInspectionOptions::default());
        assert_eq!(duplicate.charged_bytes, Some(1024));
        assert_eq!(duplicate.journals_checked, 1);
        assert!(!duplicate.recovery_authorized);
        assert!(!duplicate.writer_termination_verified);
        assert_eq!(fs::read(store.lease.root().join(MANIFEST)).unwrap(), before);
        let revision = store.list("configured-api", &"b".repeat(64)).unwrap()[0].revision;
        let outcome = store
            .recover_observed(
                &id,
                &identity(),
                revision,
                &control(),
                |_| false,
                |point| {
                    if point == RecoveryPoint::BeforeJournalCleanup {
                        return Err(CacheError::Io(io::Error::other("retain terminal journal")));
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(outcome.recovered && outcome.journal_cleanup_pending);
        let manifest = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        assert!(
            !decode_manifest(&manifest, 100)
                .unwrap()
                .entries
                .contains_key(&id)
        );
        let observed =
            inspect_snapshot_store(store.lease.root(), &SnapshotInspectionOptions::default());
        assert_eq!(observed.charged_bytes, Some(1024));
        assert_eq!(observed.journals_checked, 1);
        assert_eq!(observed.records_checked, 1);
        assert!(!observed.validation_complete);
        assert!(observed.read_only && observed.no_recovery_authority);
        assert_eq!(
            fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            manifest
        );
        let bounded = inspect_snapshot_store(
            store.lease.root(),
            &SnapshotInspectionOptions {
                maximum_manifest_bytes: manifest.len() as u64 + 1,
                ..Default::default()
            },
        );
        assert_eq!(bounded.charged_bytes, None);
        assert!(
            bounded
                .issues
                .iter()
                .any(|issue| issue.code == "recovery_journal_size_limit")
        );
        let journal = fs::read_dir(store.lease.root())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(JOURNAL_PREFIX)
            })
            .unwrap()
            .path();
        fs::write(&journal, br#"{"schema_version":999}"#).unwrap();
        let invalid =
            inspect_snapshot_store(store.lease.root(), &SnapshotInspectionOptions::default());
        assert_eq!(invalid.charged_bytes, None);
        assert!(
            invalid
                .issues
                .iter()
                .any(|issue| issue.code == "recovery_journal_invalid")
        );
        assert!(!invalid.recovery_authorized);
    }

    #[test]
    #[cfg(unix)]
    fn root_loss_after_durable_journal_does_not_unlink_a_replacement_roots_files() {
        let (_directory, store) = fixture();
        let pending = bound(&store);
        let id = pending.id().to_owned();
        fs::write(pending.path(), b"retained original").unwrap();
        drop(pending);
        store.acknowledge_writer_exit("model-one", 7);
        let root = store.lease.root().to_owned();
        let moved = root.with_file_name("retained-moved-cache");
        let error = store
            .recover_observed(
                &id,
                &identity(),
                1,
                &control(),
                |_| false,
                |point| {
                    if point == RecoveryPoint::BeforeUnlink {
                        fs::rename(&root, &moved).unwrap();
                        fs::create_dir(&root).unwrap();
                        fs::write(root.join(format!("kv-{id}.bin")), b"foreign replacement")
                            .unwrap();
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(matches!(error, CacheError::RootUnavailable));
        assert_eq!(fs::read(store.path(&id)).unwrap(), b"foreign replacement");
        assert_eq!(
            fs::read(moved.join(format!("kv-{id}.bin"))).unwrap(),
            b"retained original"
        );
        assert!(
            store.runtime.lock().unwrap().manifest.entries[&id]
                .writer
                .as_ref()
                .unwrap()
                .cleanup_eligible
        );
        fs::remove_file(store.path(&id)).unwrap();
        fs::remove_dir(&root).unwrap();
        fs::rename(moved, root).unwrap();
        assert_eq!(charge(&store), 1024);
    }
}
