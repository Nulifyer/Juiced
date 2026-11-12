use super::*;
use writer::ActorBinding;

const JOURNAL_PREFIX: &str = ".reader-recovery-";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReaderMetadata {
    pub id: String,
    pub revision: u64,
    pub writer_current_owner: bool,
    pub writer_actor_present: bool,
    pub release_eligible: bool,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReaderIntent {
    schema_version: u32,
    purpose: ReaderPurpose,
    pub(super) snapshot: String,
    revision: u64,
    bytes: u64,
    sha256: String,
    pub(super) binding: ActorBinding,
    release_eligible: bool,
}

#[derive(Clone, Copy, Deserialize, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ReaderPurpose {
    Restore,
}

impl std::fmt::Debug for ReaderIntent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReaderIntent")
            .field("schema_version", &self.schema_version)
            .field("purpose", &self.purpose)
            .field("revision", &self.revision)
            .field("release_eligible", &self.release_eligible)
            .finish_non_exhaustive()
    }
}

impl ReaderIntent {
    pub(super) fn validate(&self, manifest: &Manifest, id: &str) -> Result<(), CacheError> {
        validate_id(id)?;
        validate_id(&self.snapshot)?;
        self.binding
            .validate_authority(manifest.store_id.as_deref())?;
        let snapshot = manifest
            .entries
            .get(&self.snapshot)
            .ok_or(CacheError::Corrupt(
                "Reader references unavailable snapshot metadata",
            ))?;
        if self.schema_version != 1
            || self.revision == 0
            || self.binding.cleanup_eligible
            || self.bytes == 0
            || snapshot.state != SnapshotState::Saved
            || snapshot.bytes != Some(self.bytes)
            || snapshot.sha256.as_ref() != Some(&self.sha256)
            || self.binding.binding_digest
                != self.binding.reader_digest(
                    id,
                    &self.snapshot,
                    &snapshot.identity,
                    self.bytes,
                    &self.sha256,
                )?
        {
            return Err(CacheError::Corrupt(
                "Reader binding does not match its saved snapshot",
            ));
        }
        Ok(())
    }
}

pub struct BoundSnapshotRead {
    read: SnapshotReadLease,
    reader_id: String,
    reader_revision: u64,
}

impl BoundSnapshotRead {
    pub fn path(&self) -> &Path {
        self.read.path()
    }
    pub fn metadata(&self) -> &SnapshotMetadata {
        self.read.metadata()
    }
    pub fn reader_id(&self) -> &str {
        &self.reader_id
    }
    pub fn reader_revision(&self) -> u64 {
        self.reader_revision
    }
}

#[derive(Debug, Serialize)]
pub struct ReaderSettlement {
    pub settled: bool,
    pub reader: String,
    pub generation: u64,
    pub journal_cleanup_pending: bool,
}

#[derive(Debug, Serialize)]
pub struct ReaderRecoveryOutcome {
    pub recovered: bool,
    pub snapshot: String,
    pub reader: String,
    pub generation: u64,
    pub journal_cleanup_pending: bool,
}

pub(super) fn metadata_for(runtime: &Runtime, snapshot: &str) -> Vec<ReaderMetadata> {
    runtime
        .manifest
        .reader_intents
        .iter()
        .filter(|(_, intent)| intent.snapshot == snapshot)
        .map(|(id, intent)| project(runtime, id, intent))
        .collect()
}

pub(super) fn project(runtime: &Runtime, id: &str, intent: &ReaderIntent) -> ReaderMetadata {
    ReaderMetadata {
        id: id.into(),
        revision: intent.revision,
        writer_current_owner: intent.binding.current_owner(&runtime.manager_owner),
        writer_actor_present: intent.binding.actor_present(),
        release_eligible: intent.release_eligible,
    }
}

#[derive(Clone, Copy)]
struct ReaderTarget<'a> {
    snapshot: &'a str,
    reader: &'a str,
    identity: &'a SnapshotIdentity,
    revision: u64,
}

impl CacheStore {
    pub fn bind_reader(
        self: &Arc<Self>,
        mut read: SnapshotReadLease,
        request: WriterRequest,
        control: &OperationControl,
    ) -> Result<BoundSnapshotRead, CacheError> {
        control.check()?;
        request.validate()?;
        if !Arc::ptr_eq(self, &read.store) {
            return Err(CacheError::NotFound);
        }
        let mut runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
        let record = runtime
            .manifest
            .entries
            .get(&read.metadata.id)
            .ok_or(CacheError::NotFound)?;
        if record.state != SnapshotState::Saved
            || record.revision != read.metadata.revision
            || record.bytes != read.metadata.bytes
            || record.sha256 != read.metadata.sha256
        {
            return Err(CacheError::Conflict);
        }
        if runtime
            .manifest
            .entries
            .len()
            .checked_add(runtime.manifest.reader_intents.len())
            .is_none_or(|count| count >= self.limits.maximum_entries)
        {
            return Err(CacheError::Limit);
        }
        if runtime
            .manifest
            .reader_intents
            .values()
            .any(|intent| intent.binding.model == request.model)
        {
            return Err(CacheError::Busy);
        }
        let id = crate::storage::generated_id()?;
        let mut binding = ActorBinding::authority(&self.store_id, &runtime.manager_owner, request)?;
        let bytes = record.bytes.ok_or(CacheError::Uncertain)?;
        let sha256 = record.sha256.clone().ok_or(CacheError::Uncertain)?;
        binding.binding_digest =
            binding.reader_digest(&id, &read.metadata.id, &record.identity, bytes, &sha256)?;
        let intent = Arc::new(ReaderIntent {
            schema_version: 1,
            purpose: ReaderPurpose::Restore,
            snapshot: read.metadata.id.clone(),
            revision: 1,
            bytes,
            sha256,
            binding,
            release_eligible: false,
        });
        let mut next = runtime.manifest.clone();
        next.store_id = Some(self.store_id.clone());
        next.reader_intents.insert(id.clone(), intent.clone());
        manifest_within_limit(&next, self.limits.maximum_manifest_bytes)?;
        self.verify_scope()?;
        control.check()?;
        intent
            .binding
            .register_receipt(&mut *self.leases.lock().map_err(|_| CacheError::Uncertain)?);
        if let Err(error) = self.commit(&mut runtime, next) {
            if matches!(error, CacheError::Persistence(_)) {
                runtime.manifest.store_id = Some(self.store_id.clone());
                runtime.manifest.reader_intents.entry(id).or_insert(intent);
            } else {
                self.prune_exit_receipt(&runtime, &intent.binding);
            }
            return Err(error);
        }
        read.metadata = metadata(
            &read.metadata.id,
            runtime
                .manifest
                .entries
                .get(&read.metadata.id)
                .expect("bound saved snapshot"),
            &runtime,
        );
        Ok(BoundSnapshotRead {
            read,
            reader_id: id,
            reader_revision: 1,
        })
    }

    pub fn validate_reader(
        &self,
        read: &BoundSnapshotRead,
        request: &WriterRequest,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        request.validate()?;
        if !std::ptr::eq(self, read.read.store.as_ref()) {
            return Err(CacheError::NotFound);
        }
        let runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
        let intent = runtime
            .manifest
            .reader_intents
            .get(&read.reader_id)
            .ok_or(CacheError::NotFound)?;
        intent.validate(&runtime.manifest, &read.reader_id)?;
        if intent.snapshot != read.metadata().id
            || intent.revision != read.reader_revision
            || intent.release_eligible
            || !intent.binding.matches(&runtime.manager_owner, request)
        {
            return Err(CacheError::Uncertain);
        }
        drop(runtime);
        self.verify_scope()?;
        control.check()
    }

    pub(crate) fn check_state_readers(
        &self,
        model: &str,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        let runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
        if runtime
            .manifest
            .reader_intents
            .values()
            .any(|intent| intent.binding.model == model)
        {
            return Err(CacheError::Busy);
        }
        Ok(())
    }

    // Only the original restore's full EOF and valid acknowledgement authorize this call.
    pub fn settle_reader(
        &self,
        read: &BoundSnapshotRead,
        control: &OperationControl,
    ) -> Result<ReaderSettlement, CacheError> {
        self.settle_observed(read, control, |_| Ok(()))
    }

    fn settle_observed(
        &self,
        read: &BoundSnapshotRead,
        control: &OperationControl,
        checkpoint: impl FnMut(ReaderPoint) -> Result<(), CacheError>,
    ) -> Result<ReaderSettlement, CacheError> {
        if !std::ptr::eq(self, read.read.store.as_ref()) {
            return Err(CacheError::NotFound);
        }
        let identity = {
            let runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
            let record = runtime
                .manifest
                .entries
                .get(&read.metadata().id)
                .ok_or(CacheError::NotFound)?;
            record.identity.clone()
        };
        let outcome = self.clear_reader(
            ReaderTarget {
                snapshot: &read.metadata().id,
                reader: &read.reader_id,
                identity: &identity,
                revision: read.reader_revision,
            },
            control,
            None,
            checkpoint,
        )?;
        Ok(ReaderSettlement {
            settled: true,
            reader: outcome.reader,
            generation: outcome.generation,
            journal_cleanup_pending: outcome.journal_cleanup_pending,
        })
    }

    pub fn recover_reader(
        &self,
        snapshot: &str,
        reader: &str,
        identity: &SnapshotIdentity,
        revision: u64,
        control: &OperationControl,
    ) -> Result<ReaderRecoveryOutcome, CacheError> {
        self.recover_reader_observed(
            ReaderTarget {
                snapshot,
                reader,
                identity,
                revision,
            },
            control,
            |binding, owner| binding.exited(self, owner, control),
            |_| Ok(()),
        )
    }

    fn recover_reader_observed(
        &self,
        target: ReaderTarget<'_>,
        control: &OperationControl,
        observe: impl FnOnce(&ActorBinding, &str) -> Result<bool, CacheError>,
        checkpoint: impl FnMut(ReaderPoint) -> Result<(), CacheError>,
    ) -> Result<ReaderRecoveryOutcome, CacheError> {
        let ReaderTarget {
            snapshot,
            reader,
            identity,
            revision,
        } = target;
        validate_id(snapshot)?;
        validate_id(reader)?;
        identity.validate()?;
        control.check()?;
        let (binding, owner, eligible) = {
            let runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
            authorized(&runtime.manifest, snapshot, identity)?;
            let intent = runtime
                .manifest
                .reader_intents
                .get(reader)
                .filter(|intent| intent.snapshot == snapshot)
                .ok_or(CacheError::NotFound)?;
            if intent.revision != revision {
                return Err(CacheError::Conflict);
            }
            intent.validate(&runtime.manifest, reader)?;
            if runtime.reader_count(snapshot) != 0 {
                return Err(CacheError::Busy);
            }
            (
                intent.binding.clone(),
                runtime.manager_owner.clone(),
                intent.release_eligible,
            )
        };
        if !eligible && !observe(&binding, &owner)? {
            return Err(CacheError::Uncertain);
        }
        control.check()?;
        self.clear_reader(target, control, Some(binding.binding_digest), checkpoint)
    }

    fn clear_reader(
        &self,
        target: ReaderTarget<'_>,
        control: &OperationControl,
        expected_binding: Option<String>,
        mut checkpoint: impl FnMut(ReaderPoint) -> Result<(), CacheError>,
    ) -> Result<ReaderRecoveryOutcome, CacheError> {
        let ReaderTarget {
            snapshot,
            reader,
            identity,
            revision,
        } = target;
        control.check()?;
        let retained = {
            let mut runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
            authorized(&runtime.manifest, snapshot, identity)?;
            let intent = runtime
                .manifest
                .reader_intents
                .get(reader)
                .filter(|intent| intent.snapshot == snapshot)
                .ok_or(CacheError::NotFound)?;
            if intent.revision != revision {
                return Err(CacheError::Conflict);
            }
            intent.validate(&runtime.manifest, reader)?;
            if expected_binding
                .as_ref()
                .is_some_and(|digest| *digest != intent.binding.binding_digest)
            {
                return Err(CacheError::Uncertain);
            }
            if expected_binding.is_some() && runtime.reader_count(snapshot) != 0 {
                return Err(CacheError::Busy);
            }
            let mut retained = intent.clone();
            if !retained.release_eligible {
                let changed = Arc::make_mut(&mut retained);
                changed.release_eligible = true;
                changed.revision = changed.revision.checked_add(1).ok_or(CacheError::Limit)?;
                let mut next = runtime.manifest.clone();
                next.reader_intents.insert(reader.into(), retained.clone());
                self.verify_scope()?;
                control.check()?;
                self.commit(&mut runtime, next)?;
            }
            retained
        };
        let journal = ReaderJournal::new(&self.store_id, reader, retained.clone());
        self.ensure_reader_journal(&journal, control)?;
        checkpoint(ReaderPoint::JournalDurable)?;
        control.check()?;
        self.verify_scope()?;
        let mut runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
        let current = runtime
            .manifest
            .reader_intents
            .get(reader)
            .ok_or(CacheError::NotFound)?;
        if current != &retained {
            return Err(CacheError::Conflict);
        }
        let mut next = runtime.manifest.clone();
        next.reader_intents.remove(reader);
        checkpoint(ReaderPoint::BeforeFinalCommit)?;
        control.check()?;
        if let Err(error) = self
            .commit(&mut runtime, next)
            .and_then(|_| checkpoint(ReaderPoint::AfterFinalCommit))
        {
            runtime
                .manifest
                .reader_intents
                .entry(reader.into())
                .or_insert(retained);
            return Err(error);
        }
        let journal_cleanup_pending = checkpoint(ReaderPoint::BeforeJournalCleanup)
            .and_then(|_| self.remove_reader_journal(&journal, &runtime.manifest, control))
            .is_err();
        if journal_cleanup_pending {
            runtime
                .manifest
                .reader_intents
                .entry(reader.into())
                .or_insert(journal.intent.clone());
        }
        self.prune_exit_receipt(&runtime, &journal.intent.binding);
        Ok(ReaderRecoveryOutcome {
            recovered: true,
            snapshot: snapshot.into(),
            reader: reader.into(),
            generation: runtime.manifest.generation,
            journal_cleanup_pending,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderPoint {
    JournalDurable,
    BeforeFinalCommit,
    AfterFinalCommit,
    BeforeJournalCleanup,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReaderJournal {
    schema_version: u32,
    store_id: String,
    reader_id: String,
    intent: Arc<ReaderIntent>,
}

impl ReaderJournal {
    fn new(store: &str, id: &str, intent: Arc<ReaderIntent>) -> Self {
        Self {
            schema_version: 1,
            store_id: store.into(),
            reader_id: id.into(),
            intent,
        }
    }
    fn filename(&self) -> String {
        format!(
            "{JOURNAL_PREFIX}{}-{}.json",
            self.intent.snapshot, self.reader_id
        )
    }
    fn validate(&self, filename: &str, manifest: &Manifest) -> Result<(), CacheError> {
        if self.schema_version != 1
            || Some(self.store_id.as_str()) != manifest.store_id.as_deref()
            || self.filename() != filename
            || !self.intent.release_eligible
        {
            return Err(CacheError::Corrupt("Reader recovery journal is invalid"));
        }
        self.intent.validate(manifest, &self.reader_id)
    }
}

pub(super) fn reader_journal_name(name: &str) -> bool {
    name.starts_with(JOURNAL_PREFIX)
}
pub(super) fn reader_journal_name_valid(name: &str) -> bool {
    name.strip_prefix(JOURNAL_PREFIX)
        .and_then(|name| name.strip_suffix(".json"))
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(snapshot, reader)| {
            validate_id(snapshot).is_ok() && writer::opaque_valid(reader)
        })
}

pub(super) fn decode_reader_journal(
    bytes: &[u8],
    filename: &str,
    manifest: &mut Manifest,
    maximum_entries: usize,
    _maximum_snapshot_bytes: u64,
) -> Result<(), CacheError> {
    if bytes.len() as u64 > writer::maximum_recovery_journal_bytes()
        || !reader_journal_name_valid(filename)
    {
        return Err(CacheError::Limit);
    }
    let journal: ReaderJournal = serde_json::from_slice(bytes)
        .map_err(|_| CacheError::Corrupt("Reader recovery journal is unreadable"))?;
    journal.validate(filename, manifest)?;
    match manifest.reader_intents.get(&journal.reader_id) {
        Some(intent) if intent != &journal.intent => Err(CacheError::Corrupt(
            "Reader journal conflicts with its retained barrier",
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
                .reader_intents
                .insert(journal.reader_id, journal.intent);
            Ok(())
        }
    }
}

impl CacheStore {
    fn ensure_reader_journal(
        &self,
        journal: &ReaderJournal,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        self.verify_scope()?;
        let filename = journal.filename();
        match self
            .lease
            .read_bytes(&filename, writer::maximum_recovery_journal_bytes())
        {
            Ok(bytes) => {
                let previous: ReaderJournal = serde_json::from_slice(&bytes)
                    .map_err(|_| CacheError::Corrupt("Reader recovery journal is unreadable"))?;
                let runtime = self.runtime.lock().map_err(|_| CacheError::Uncertain)?;
                previous.validate(&filename, &runtime.manifest)?;
                if previous.intent != journal.intent {
                    return Err(CacheError::Conflict);
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let content = serde_json::to_vec(journal).map_err(|_| {
                    CacheError::Corrupt("Reader recovery journal cannot be encoded")
                })?;
                let (count, bytes) = writer::recovery_journal_usage(&self.lease, &self.limits)?;
                if content.len() as u64 + 1 > writer::maximum_recovery_journal_bytes()
                    || count >= self.limits.maximum_entries
                    || bytes
                        .checked_add(content.len() as u64 + 1)
                        .is_none_or(|bytes| bytes > self.limits.maximum_manifest_bytes)
                {
                    return Err(CacheError::Limit);
                }
                control.check()?;
                self.scope
                    .write_json(&self.lease, filename, journal)
                    .map_err(CacheError::Persistence)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn remove_reader_journal(
        &self,
        journal: &ReaderJournal,
        manifest: &Manifest,
        control: &OperationControl,
    ) -> Result<(), CacheError> {
        control.check()?;
        self.verify_scope()?;
        let filename = journal.filename();
        let bytes = self
            .lease
            .read_bytes(&filename, writer::maximum_recovery_journal_bytes())?;
        let previous: ReaderJournal = serde_json::from_slice(&bytes)
            .map_err(|_| CacheError::Corrupt("Reader recovery journal is unreadable"))?;
        previous.validate(&filename, manifest)?;
        if previous.intent != journal.intent {
            return Err(CacheError::Conflict);
        }
        let path = self.lease.file_path(filename)?;
        let file = regular_file(&path)?;
        control.check()?;
        self.verify_scope()?;
        if !same_file(&file, &path)? {
            return Err(CacheError::Uncertain);
        }
        fs::remove_file(path)?;
        self.sync_recovery_directory(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{fixture, identity, saved};

    fn control() -> OperationControl {
        OperationControl::new(
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        )
    }
    fn request() -> WriterRequest {
        WriterRequest {
            model: "reader-model".into(),
            generation: 9,
            actor: None,
        }
    }
    fn bound(store: &Arc<CacheStore>, snapshot: &str) -> BoundSnapshotRead {
        let read = store
            .restore_controlled(snapshot, &identity(), &control())
            .unwrap();
        store.bind_reader(read, request(), &control()).unwrap()
    }
    fn rows(store: &CacheStore) -> (u64, Vec<SnapshotMetadata>) {
        store.inventory("configured-api", &"b".repeat(64)).unwrap()
    }

    #[test]
    fn durable_reader_precedes_dispatch_and_drop_never_settles_or_waits_for_manifest_io() {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let bytes = fs::read(store.path(&snapshot.id)).unwrap();
        let read = bound(&store, &snapshot.id);
        let reader_id = read.reader_id().to_owned();
        store
            .validate_reader(&read, &request(), &control())
            .unwrap();
        let persisted = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let manifest = decode_manifest(&persisted, 100).unwrap();
        assert_eq!(manifest.reader_intents[&reader_id].snapshot, snapshot.id);
        assert!(!manifest.reader_intents[&reader_id].release_eligible);
        let lock = store.runtime.lock().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            drop(read);
            let _ = sender.send(());
        });
        let dropped = receiver.recv_timeout(Duration::from_secs(1));
        drop(lock);
        thread.join().unwrap();
        dropped.expect("bound reader Drop waited for manifest I/O");
        assert_eq!(
            fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            persisted
        );
        let restarted = CacheStore::open(store.lease.clone()).unwrap();
        assert_eq!(rows(&restarted).1[0].active_readers, 0);
        assert_eq!(rows(&restarted).1[0].retained_readers[0].id, reader_id);
        assert!(matches!(
            restarted.forget(&snapshot.id, &identity(), snapshot.revision),
            Err(CacheError::Busy)
        ));
        assert!(matches!(
            restarted.recover_reader(&snapshot.id, &reader_id, &identity(), 1, &control()),
            Err(CacheError::Uncertain)
        ));
        assert!(matches!(
            restarted.check_state_readers("reader-model", &control()),
            Err(CacheError::Busy)
        ));
        restarted
            .check_state_readers("independent-model", &control())
            .unwrap();
        assert_eq!(fs::read(store.path(&snapshot.id)).unwrap(), bytes);
    }

    #[test]
    fn current_reader_recovery_requires_exact_exit_and_preserves_saved_snapshot() {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let bytes = fs::read(store.path(&snapshot.id)).unwrap();
        let read = bound(&store, &snapshot.id);
        let id = read.reader_id().to_owned();
        let mut wrong_request = request();
        wrong_request.generation = 10;
        assert!(matches!(
            store.validate_reader(&read, &wrong_request, &control()),
            Err(CacheError::Uncertain)
        ));
        let mut stranger = identity();
        stranger.principal = "other".into();
        assert!(matches!(
            store.recover_reader(&snapshot.id, &id, &stranger, 1, &control()),
            Err(CacheError::NotFound)
        ));
        assert!(matches!(
            store.recover_reader(&snapshot.id, &id, &identity(), 99, &control()),
            Err(CacheError::Conflict)
        ));
        assert!(matches!(
            store.recover_reader(&"f".repeat(32), &id, &identity(), 1, &control()),
            Err(CacheError::NotFound)
        ));
        store.acknowledge_writer_exit("reader-model", 9);
        assert!(matches!(
            store.recover_reader(&snapshot.id, &id, &identity(), 1, &control()),
            Err(CacheError::Busy)
        ));
        drop(read);
        let recovered = store
            .recover_reader(&snapshot.id, &id, &identity(), 1, &control())
            .unwrap();
        assert!(recovered.recovered && !recovered.journal_cleanup_pending);
        let (generation, inventory) = rows(&store);
        assert_eq!(recovered.generation, generation);
        assert!(inventory[0].retained_readers.is_empty());
        assert_eq!(inventory[0].revision, snapshot.revision);
        assert_eq!(inventory[0].state, SnapshotState::Saved);
        assert_eq!(fs::read(store.path(&snapshot.id)).unwrap(), bytes);
    }

    #[test]
    fn explicit_reader_settlement_is_durable_and_cancellation_cannot_clear_the_barrier() {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let read = bound(&store, &snapshot.id);
        let cancelled = control();
        cancelled.cancelled().store(true, Ordering::Release);
        assert!(matches!(
            store.settle_reader(&read, &cancelled),
            Err(CacheError::Cancelled)
        ));
        assert_eq!(rows(&store).1[0].retained_readers.len(), 1);
        let settled = store.settle_reader(&read, &control()).unwrap();
        assert!(settled.settled && !settled.journal_cleanup_pending);
        assert!(rows(&store).1[0].retained_readers.is_empty());
        assert_eq!(rows(&store).1[0].active_readers, 1);
        drop(read);
        let restarted = CacheStore::open(store.lease.clone()).unwrap();
        assert!(rows(&restarted).1[0].retained_readers.is_empty());
        assert_eq!(rows(&restarted).1[0].active_readers, 0);
        assert!(store.path(&snapshot.id).exists());
    }

    #[test]
    fn committed_settlement_failure_retains_reader_journal_across_reopen_without_deleting_saved_bytes()
     {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let bytes = fs::read(store.path(&snapshot.id)).unwrap();
        let read = bound(&store, &snapshot.id);
        let id = read.reader_id().to_owned();
        let error = store
            .settle_observed(&read, &control(), |point| {
                if point == ReaderPoint::AfterFinalCommit {
                    let actual =
                        decode_manifest(&fs::read(store.lease.root().join(MANIFEST)).unwrap(), 100)
                            .unwrap();
                    assert!(!actual.reader_intents.contains_key(&id));
                    return Err(CacheError::Persistence(AtomicWriteError::test_committed(
                        crate::storage::WriteStage::SyncDirectory,
                        io::Error::other(
                            "controlled failure after actual reader settlement commit",
                        ),
                    )));
                }
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(error, CacheError::Persistence(error) if error.committed()));
        drop(read);
        let actual_head = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let restarted = CacheStore::open(store.lease.clone()).unwrap();
        assert_eq!(
            fs::read(store.lease.root().join(MANIFEST)).unwrap(),
            actual_head
        );
        let metadata = &rows(&restarted).1[0];
        assert_eq!(metadata.retained_readers.len(), 1);
        assert!(metadata.retained_readers[0].release_eligible);
        assert!(matches!(
            restarted.forget(&snapshot.id, &identity(), snapshot.revision),
            Err(CacheError::Busy)
        ));
        let report =
            inspect_snapshot_store(store.lease.root(), &SnapshotInspectionOptions::default());
        assert_eq!(report.retained_readers, 1);
        assert_eq!(report.charged_bytes, Some(bytes.len() as u64));
        assert!(!report.validation_complete && !report.recovery_authorized);
        restarted
            .recover_reader(
                &snapshot.id,
                &id,
                &identity(),
                metadata.retained_readers[0].revision,
                &control(),
            )
            .unwrap();
        assert!(rows(&restarted).1[0].retained_readers.is_empty());
        assert_eq!(fs::read(store.path(&snapshot.id)).unwrap(), bytes);
        assert_eq!(rows(&restarted).1[0].state, SnapshotState::Saved);
    }

    #[test]
    fn prior_reader_actor_requires_positive_observation_and_never_uses_current_manager_ack() {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let data = fs::read(store.path(&snapshot.id)).unwrap();
        let actor = serde_json::from_value(serde_json::json!({"version":1,"method":"future-observe-only","pid":42,"domain":"d".repeat(64),"token":"e".repeat(64)})).unwrap();
        let mut captured = request();
        captured.actor = Some(actor);
        let read = store.restore(&snapshot.id, &identity()).unwrap();
        let bound = store.bind_reader(read, captured, &control()).unwrap();
        let id = bound.reader_id().to_owned();
        drop(bound);
        let restarted = CacheStore::open(store.lease.clone()).unwrap();
        restarted.acknowledge_writer_exit("reader-model", 9);
        assert!(matches!(
            restarted.recover_reader(&snapshot.id, &id, &identity(), 1, &control()),
            Err(CacheError::Uncertain)
        ));
        let accepted_identity = identity();
        let target = ReaderTarget {
            snapshot: &snapshot.id,
            reader: &id,
            identity: &accepted_identity,
            revision: 1,
        };
        assert!(matches!(
            restarted.recover_reader_observed(target, &control(), |_, _| Ok(false), |_| Ok(())),
            Err(CacheError::Uncertain)
        ));
        let (_, rows) = rows(&restarted);
        assert!(!rows[0].retained_readers[0].writer_current_owner);
        assert!(rows[0].retained_readers[0].writer_actor_present);
        let recovered = restarted
            .recover_reader_observed(
                target,
                &control(),
                |binding, _| {
                    assert!(binding.actor_present());
                    Ok(true)
                },
                |_| Ok(()),
            )
            .unwrap();
        assert!(recovered.recovered);
        assert_eq!(fs::read(store.path(&snapshot.id)).unwrap(), data);
        assert_eq!(rows[0].revision, snapshot.revision);
    }

    #[test]
    fn reader_purpose_snapshot_checksum_and_actor_authority_cannot_be_mixed_with_writer_bindings() {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let other_snapshot = saved(&store);
        let read = bound(&store, &snapshot.id);
        let id = read.reader_id().to_owned();
        let original = store.runtime.lock().unwrap().manifest.clone();
        for field in ["snapshot", "bytes", "sha256", "purpose"] {
            let mut value = serde_json::to_value(&original).unwrap();
            value["reader_intents"][&id][field] = match field {
                "snapshot" => serde_json::json!(other_snapshot.id),
                "bytes" => serde_json::json!(99),
                "sha256" => serde_json::json!("f".repeat(64)),
                _ => serde_json::json!("save"),
            };
            let encoded = serde_json::to_vec(&value).unwrap();
            assert!(
                decode_manifest(&encoded, 100)
                    .and_then(|manifest| validate_manifest(&manifest, 100, 2048))
                    .is_err(),
                "field={field}"
            );
        }
        for field in [
            "store_id",
            "manager_owner",
            "model",
            "generation",
            "write_key",
            "binding_digest",
        ] {
            let mut value = serde_json::to_value(&original).unwrap();
            value["reader_intents"][&id]["binding"][field] = if field == "generation" {
                serde_json::json!(88)
            } else {
                serde_json::json!("0".repeat(32))
            };
            assert!(
                decode_manifest(&serde_json::to_vec(&value).unwrap(), 100)
                    .and_then(|manifest| validate_manifest(&manifest, 100, 2048))
                    .is_err(),
                "field={field}"
            );
        }
        let mut changed = original.clone();
        Arc::make_mut(changed.reader_intents.get_mut(&id).unwrap()).binding = ActorBinding::new(
            &store.store_id,
            &store.runtime.lock().unwrap().manager_owner,
            &snapshot.id,
            &identity(),
            snapshot.reserved_bytes,
            request(),
        )
        .unwrap();
        assert!(validate_manifest(&changed, 100, 2048).is_err());
        let mut unknown = serde_json::to_value(&original).unwrap();
        unknown["reader_intents"][&id]["path"] = serde_json::json!("../unowned");
        assert!(decode_manifest(&serde_json::to_vec(&unknown).unwrap(), 100).is_err());
        let intent = &original.reader_intents[&id];
        let projected = rows(&store)
            .1
            .into_iter()
            .find(|row| row.id == snapshot.id)
            .unwrap();
        let metadata = serde_json::to_string(&projected.retained_readers).unwrap();
        assert!(!metadata.contains(&intent.binding.manager_owner));
        assert!(!metadata.contains(&intent.binding.write_key));
        assert_eq!(projected.retained_readers[0].id, id);
    }

    #[test]
    fn reader_count_deadline_and_cancel_bounds_refuse_before_intent_publication() {
        let (_directory, mut store) = fixture();
        let snapshot = saved(&store);
        Arc::get_mut(&mut store).unwrap().limits.maximum_entries = 1;
        let before = fs::read(store.lease.root().join(MANIFEST)).unwrap();
        let read = store.restore(&snapshot.id, &identity()).unwrap();
        assert!(matches!(
            store.bind_reader(read, request(), &control()),
            Err(CacheError::Limit)
        ));
        assert_eq!(rows(&store).1[0].active_readers, 0);
        let expired = OperationControl::new(Instant::now(), Arc::new(AtomicBool::new(false)));
        let read = store.restore(&snapshot.id, &identity()).unwrap();
        assert!(matches!(
            store.bind_reader(read, request(), &expired),
            Err(CacheError::Deadline)
        ));
        let cancelled = control();
        cancelled.cancelled().store(true, Ordering::Release);
        let read = store.restore(&snapshot.id, &identity()).unwrap();
        assert!(matches!(
            store.bind_reader(read, request(), &cancelled),
            Err(CacheError::Cancelled)
        ));
        assert_eq!(fs::read(store.lease.root().join(MANIFEST)).unwrap(), before);
        assert_eq!(rows(&store).1[0].active_readers, 0);
        assert!(rows(&store).1[0].retained_readers.is_empty());
    }

    #[test]
    fn reader_finalization_serializes_forget_until_journal_housekeeping_finishes() {
        let (_directory, store) = fixture();
        let snapshot = saved(&store);
        let read = bound(&store, &snapshot.id);
        let id = read.reader_id().to_owned();
        drop(read);
        store.acknowledge_writer_exit("reader-model", 9);
        let accepted_identity = identity();
        let target = ReaderTarget {
            snapshot: &snapshot.id,
            reader: &id,
            identity: &accepted_identity,
            revision: 1,
        };
        let mut competing = None;
        let result = store
            .recover_reader_observed(
                target,
                &control(),
                |binding, owner| binding.exited(&store, owner, &control()),
                |point| {
                    if point == ReaderPoint::BeforeJournalCleanup {
                        assert!(
                            store.runtime.try_lock().is_err(),
                            "saved snapshot became forgettable during journal finalization"
                        );
                        let owner = store.clone();
                        let snapshot_id = snapshot.id.clone();
                        let revision = snapshot.revision;
                        competing = Some(std::thread::spawn(move || {
                            owner.forget(&snapshot_id, &identity(), revision)
                        }));
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(result.recovered && !result.journal_cleanup_pending);
        competing.unwrap().join().unwrap().unwrap();
        let restarted = CacheStore::open(store.lease.clone()).unwrap();
        assert!(rows(&restarted).1.is_empty());
        assert!(
            !fs::read_dir(store.lease.root())
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(JOURNAL_PREFIX))
        );
    }
}
