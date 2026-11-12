use super::{
    SourceCohort,
    descriptor::{Aliases, Binding, Blob, Digest, Files, Generation, Id, Presence},
};
use crate::{
    native_settings::{Document, Role},
    storage::{DirectoryLease, LiveRoots, MetadataFile},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::Path,
    sync::{Arc, Mutex},
};

pub const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_STORE_FILES: usize = 256;
pub const MAX_INTENTS: usize = 32;
const METADATA_BYTES: u64 = 64 * 1024;
const BLOB_BYTES: u64 = 16 * 1024 * 1024;
const SPACE_MARGIN: u64 = 1024 * 1024;
const IDENTITY: &str = "identity.json";
const ROLES: [Role; 7] = [
    Role::Config,
    Role::Jobs,
    Role::RecipeOptions,
    Role::UserModels,
    Role::ExecutionProfiles,
    Role::NativeHelpers,
    Role::Aliases,
];

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Error {
    Busy,
    Closed,
    Unavailable,
    RootUnavailable,
    Unsupported,
    IdentityMissing,
    IdentityMismatch,
    InvalidStore,
    InvalidIntent,
    InvalidBinding,
    BlobChanged,
    SourceChanged,
    InvalidEdit,
    StaleDocument,
    Limit,
    Space,
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Busy => "native_settings_store_busy",
            Self::Closed => "shutting_down",
            Self::Limit => "native_settings_store_limit",
            Self::Space => "native_settings_store_space",
            Self::RootUnavailable => "storage_root_unavailable",
            _ => "configuration_migration_recovery_required",
        }
    }
    fn reason(self) -> &'static str {
        match self {
            Self::Busy => "owner_busy",
            Self::Closed => "store_closed",
            Self::Unavailable => "storage_unavailable",
            Self::RootUnavailable => "root_unavailable",
            Self::Unsupported => "platform_unsupported",
            Self::IdentityMissing => "identity_missing",
            Self::IdentityMismatch => "identity_mismatch",
            Self::InvalidStore => "store_invalid",
            Self::InvalidIntent => "intent_invalid",
            Self::InvalidBinding => "binding_invalid",
            Self::BlobChanged => "blob_changed",
            Self::SourceChanged => "source_changed",
            Self::InvalidEdit => "edit_invalid",
            Self::StaleDocument => "document_stale",
            Self::Limit => "store_limit",
            Self::Space => "space_unavailable",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: reason={}; Preserve the source and settings store.",
            self.code(),
            self.reason()
        )
    }
}
impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSettingsStoreError")
            .field("code", &self.code())
            .field("reason", &self.reason())
            .field("message", &"Preserve the source and settings store.")
            .finish()
    }
}
impl std::error::Error for Error {}
fn storage(error: io::Error) -> Error {
    if crate::storage::is_root_unavailable(&error) {
        return Error::RootUnavailable;
    }
    match error.kind() {
        io::ErrorKind::Unsupported => Error::Unsupported,
        io::ErrorKind::WouldBlock => Error::Busy,
        _ => Error::Unavailable,
    }
}
fn blob_error(error: io::Error) -> Error {
    if crate::storage::is_root_unavailable(&error) {
        Error::RootUnavailable
    } else {
        Error::BlobChanged
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema_version: u32,
    store_id: Id,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema_version: u32,
    transaction_id: Id,
    base_config_digest: Digest,
    candidate: Binding,
}

#[derive(Clone)]
struct Entry {
    bytes: u64,
    persisted: bool,
}
struct State {
    entries: BTreeMap<String, Entry>,
    metadata: Vec<Arc<MetadataFile>>,
    charged_bytes: u64,
    intents: usize,
    preparing: bool,
    closed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub charged_bytes: u64,
    pub reserved_files: usize,
    pub intents: usize,
    pub preparing: bool,
    pub closed: bool,
}

pub struct Store {
    root: DirectoryLease,
    scope: Arc<LiveRoots>,
    identity: MetadataFile,
    store_id: Id,
    instance: Arc<()>,
    state: Mutex<State>,
}
pub struct PreparedBinding {
    binding: Binding,
    identity: MetadataFile,
    blobs: BTreeMap<Id, Arc<MetadataFile>>,
    store_instance: Arc<()>,
}
pub struct PlannedImport<'a> {
    store: &'a Store,
    source: &'a SourceCohort,
    intent: Intent,
    payloads: Vec<(Blob, Arc<Vec<u8>>)>,
}
pub struct PlannedEdit<'a> {
    store: &'a Store,
    active: &'a PreparedBinding,
    intent: Option<Intent>,
    payloads: Vec<(Blob, Arc<Vec<u8>>)>,
    base_digest: Digest,
}
impl PlannedEdit<'_> {
    pub fn binding(&self) -> &Binding {
        self.intent
            .as_ref()
            .map_or(self.active.binding(), |intent| &intent.candidate)
    }
    pub fn base_config_digest(&self) -> &Digest {
        &self.base_digest
    }
    pub fn is_changed(&self) -> bool {
        self.intent.is_some()
    }
}
impl PlannedImport<'_> {
    pub fn binding(&self) -> &Binding {
        &self.intent.candidate
    }
    pub fn base_config_digest(&self) -> &Digest {
        &self.intent.base_config_digest
    }
}
impl PreparedBinding {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn file(&self, role: Role) -> Option<&MetadataFile> {
        self.binding
            .current()
            .files()
            .get(role)?
            .blob()
            .and_then(|blob| self.blobs.get(blob.id()))
            .map(Arc::as_ref)
    }
    pub fn original_file(&self, role: Role) -> Option<&MetadataFile> {
        self.binding
            .original()
            .files()
            .get(role)?
            .blob()
            .and_then(|blob| self.blobs.get(blob.id()))
            .map(Arc::as_ref)
    }
    pub fn verify(&self) -> Result<(), Error> {
        self.identity.verify().map_err(storage)?;
        let identity: Identity =
            serde_json::from_slice(self.identity.bytes()).map_err(|_| Error::InvalidStore)?;
        if identity.schema_version != 1 || identity.store_id != *self.binding.store_id() {
            return Err(Error::IdentityMismatch);
        }
        for file in self.blobs.values() {
            file.verify().map_err(blob_error)?;
        }
        Ok(())
    }
}

fn id() -> Result<Id, Error> {
    Id::new(crate::storage::generated_id().map_err(storage)?).map_err(|_| Error::InvalidStore)
}
fn digest(bytes: &[u8]) -> Digest {
    Digest::new(format!("{:x}", Sha256::digest(bytes))).expect("SHA256 produces a lowercase digest")
}
fn blob_name(id: &Id) -> String {
    format!("blob-{}", id.as_str())
}
fn intent_name(id: &Id) -> String {
    format!("intent-{}.json", id.as_str())
}
fn intent_id(name: &str) -> Option<Id> {
    Id::new(name.strip_prefix("intent-")?.strip_suffix(".json")?).ok()
}
fn blob_id(name: &str) -> Option<Id> {
    Id::new(name.strip_prefix("blob-")?).ok()
}
pub(crate) fn metadata_entry_limit(name: &str) -> Option<u64> {
    if name == IDENTITY || intent_id(name).is_some() {
        Some(METADATA_BYTES)
    } else if blob_id(name).is_some() {
        Some(BLOB_BYTES)
    } else {
        None
    }
}
fn encode<T: Serialize>(value: &T) -> Result<Arc<Vec<u8>>, Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidIntent)?;
    if bytes.len() as u64 > METADATA_BYTES {
        return Err(Error::Limit);
    }
    Ok(Arc::new(bytes))
}
fn references(binding: &Binding, output: &mut BTreeMap<Id, Blob>) -> Result<(), Error> {
    for generation in [binding.original(), binding.current()] {
        for role in ROLES {
            if let Some(blob) = generation.files().get(role).and_then(Presence::blob)
                && let Some(previous) = output.insert(blob.id().clone(), blob.clone())
                && previous != *blob
            {
                return Err(Error::InvalidBinding);
            }
        }
    }
    Ok(())
}
fn add_bytes(total: &mut u64, bytes: u64) -> Result<(), Error> {
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= MAX_STORE_BYTES)
        .ok_or(Error::Limit)?;
    Ok(())
}

impl Store {
    pub fn open(
        root: DirectoryLease,
        scope: Arc<LiveRoots>,
        active: Option<&Binding>,
        initialize: bool,
    ) -> Result<Self, Error> {
        Self::open_bound(root, scope, active, initialize).map(|(store, _)| store)
    }
    pub fn open_bound(
        root: DirectoryLease,
        scope: Arc<LiveRoots>,
        active: Option<&Binding>,
        initialize: bool,
    ) -> Result<(Self, Option<PreparedBinding>), Error> {
        let inventory = root
            .private_metadata_entries(&scope, MAX_STORE_FILES)
            .map_err(storage)?;
        let mut entries = BTreeMap::new();
        for (name, bytes) in inventory {
            let name = name.into_string().map_err(|_| Error::InvalidStore)?;
            if metadata_entry_limit(&name).is_none() {
                return Err(Error::InvalidStore);
            }
            if entries
                .insert(
                    name,
                    Entry {
                        bytes,
                        persisted: true,
                    },
                )
                .is_some()
            {
                return Err(Error::InvalidStore);
            }
        }
        let identity = if entries.contains_key(IDENTITY) {
            root.read_private_metadata(scope.clone(), Path::new(IDENTITY), METADATA_BYTES)
                .map_err(storage)?
        } else {
            if !initialize || active.is_some() || !entries.is_empty() {
                return Err(Error::IdentityMissing);
            }
            let encoded = encode(&Identity {
                schema_version: 1,
                store_id: id()?,
            })?;
            let file = root
                .write_new_private_metadata(scope.clone(), Path::new(IDENTITY), encoded)
                .map_err(storage)?;
            entries.insert(
                IDENTITY.into(),
                Entry {
                    bytes: file.bytes().len() as u64,
                    persisted: true,
                },
            );
            file
        };
        identity.try_lock_exclusive().map_err(storage)?;
        let decoded: Identity =
            serde_json::from_slice(identity.bytes()).map_err(|_| Error::InvalidStore)?;
        if decoded.schema_version != 1 {
            return Err(Error::InvalidStore);
        }
        if active.is_some_and(|binding| binding.store_id() != &decoded.store_id) {
            return Err(Error::IdentityMismatch);
        }
        let mut metadata = Vec::new();
        let mut refs = BTreeMap::new();
        let mut intents = 0usize;
        if let Some(binding) = active {
            references(binding, &mut refs)?;
        }
        let active_refs = refs.clone();
        let mut active_files = BTreeMap::new();
        for name in entries.keys().filter(|name| intent_id(name).is_some()) {
            intents += 1;
            if intents > MAX_INTENTS {
                return Err(Error::Limit);
            }
            let file = root
                .read_private_metadata(scope.clone(), Path::new(name), METADATA_BYTES)
                .map_err(storage)?;
            let intent: Intent =
                serde_json::from_slice(file.bytes()).map_err(|_| Error::InvalidIntent)?;
            if intent.schema_version != 1
                || Some(&intent.transaction_id) != intent_id(name).as_ref()
                || intent.candidate.store_id() != &decoded.store_id
                || intent.candidate.current().id() != &intent.transaction_id
            {
                return Err(Error::InvalidIntent);
            }
            references(&intent.candidate, &mut refs)?;
            metadata.push(Arc::new(file));
        }
        let mut charged = METADATA_BYTES;
        for _ in 0..intents {
            add_bytes(&mut charged, METADATA_BYTES)?;
        }
        for (id, blob) in &refs {
            let name = blob_name(id);
            let mut reservation = blob.bytes();
            if let Some(entry) = entries.get(&name) {
                if entry.bytes > BLOB_BYTES {
                    return Err(Error::Limit);
                }
                if active_refs.contains_key(id) {
                    if entry.bytes != blob.bytes() {
                        return Err(Error::BlobChanged);
                    }
                    let file = root
                        .read_private_metadata(scope.clone(), Path::new(&name), BLOB_BYTES)
                        .map_err(storage)?;
                    if digest(file.bytes()) != *blob.sha256() {
                        return Err(Error::BlobChanged);
                    }
                    active_files.insert(id.clone(), Arc::new(file));
                } else if entry.bytes > blob.bytes() {
                    reservation = BLOB_BYTES;
                }
            } else {
                if active_refs.contains_key(id) {
                    return Err(Error::BlobChanged);
                }
                entries.insert(
                    name,
                    Entry {
                        bytes: blob.bytes(),
                        persisted: false,
                    },
                );
            }
            add_bytes(&mut charged, reservation)?;
        }
        for (name, entry) in &entries {
            if let Some(id) = blob_id(name)
                && !refs.contains_key(&id)
            {
                if entry.bytes > BLOB_BYTES {
                    return Err(Error::Limit);
                }
                add_bytes(&mut charged, BLOB_BYTES)?;
            }
        }
        if entries.len() > MAX_STORE_FILES {
            return Err(Error::Limit);
        }
        let store = Self {
            root,
            scope,
            identity,
            store_id: decoded.store_id,
            instance: Arc::new(()),
            state: Mutex::new(State {
                entries,
                metadata,
                charged_bytes: charged,
                intents,
                preparing: false,
                closed: false,
            }),
        };
        store.verify_inventory()?;
        let bound = active
            .map(|binding| {
                let identity = store
                    .root
                    .read_private_metadata(store.scope.clone(), Path::new(IDENTITY), METADATA_BYTES)
                    .map_err(storage)?;
                let prepared = PreparedBinding {
                    binding: binding.clone(),
                    identity,
                    blobs: active_files,
                    store_instance: store.instance.clone(),
                };
                prepared.verify()?;
                Ok(prepared)
            })
            .transpose()?;
        store.identity.verify().map_err(storage)?;
        Ok((store, bound))
    }

    pub fn store_id(&self) -> &Id {
        &self.store_id
    }
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        let state = self.state.lock().map_err(|_| Error::Unavailable)?;
        Ok(Snapshot {
            charged_bytes: state.charged_bytes,
            reserved_files: state.entries.len(),
            intents: state.intents,
            preparing: state.preparing,
            closed: state.closed,
        })
    }
    pub fn close(&self) -> Result<(), Error> {
        self.state.lock().map_err(|_| Error::Unavailable)?.closed = true;
        Ok(())
    }
    pub fn verify_retained(&self) -> Result<(), Error> {
        let _preparing = self.begin()?;
        self.verify_inventory()
    }
    pub fn read_binding(&self, binding: &Binding) -> Result<PreparedBinding, Error> {
        let _preparing = self.begin()?;
        self.verify_inventory()?;
        let blobs = self.retain_blobs(binding)?;
        let identity = self
            .root
            .read_private_metadata(self.scope.clone(), Path::new(IDENTITY), METADATA_BYTES)
            .map_err(storage)?;
        let prepared = PreparedBinding {
            binding: binding.clone(),
            identity,
            blobs,
            store_instance: self.instance.clone(),
        };
        prepared.verify()?;
        self.verify_inventory()?;
        Ok(prepared)
    }

    fn verify_inventory(&self) -> Result<(), Error> {
        self.identity.verify().map_err(storage)?;
        let (expected, metadata) = {
            let state = self.state.lock().map_err(|_| Error::Unavailable)?;
            (state.entries.clone(), state.metadata.clone())
        };
        for file in metadata {
            file.verify().map_err(storage)?;
        }
        let inventory = self
            .root
            .private_metadata_entries(&self.scope, MAX_STORE_FILES)
            .map_err(storage)?;
        let mut seen = BTreeSet::new();
        let mut updates = Vec::new();
        for (name, bytes) in inventory {
            let name = name.into_string().map_err(|_| Error::InvalidStore)?;
            let entry = expected.get(&name).ok_or(Error::InvalidStore)?;
            if !entry.persisted && blob_id(&name).is_some() {
                if bytes > BLOB_BYTES {
                    return Err(Error::Limit);
                }
                updates.push((
                    name.clone(),
                    bytes,
                    if bytes > entry.bytes {
                        BLOB_BYTES - entry.bytes
                    } else {
                        0
                    },
                ));
            } else if !entry.persisted || entry.bytes != bytes {
                return Err(Error::InvalidStore);
            }
            seen.insert(name);
        }
        if expected
            .iter()
            .any(|(name, entry)| entry.persisted && !seen.contains(name))
        {
            return Err(Error::InvalidStore);
        }
        self.identity.verify().map_err(storage)?;
        if !updates.is_empty() {
            let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
            let extra = updates
                .iter()
                .try_fold(0u64, |total, (_, _, charge)| total.checked_add(*charge))
                .ok_or(Error::Limit)?;
            let total = state
                .charged_bytes
                .checked_add(extra)
                .filter(|bytes| *bytes <= MAX_STORE_BYTES)
                .ok_or(Error::Limit)?;
            for (name, bytes, _) in updates {
                let entry = state.entries.get_mut(&name).ok_or(Error::InvalidStore)?;
                entry.bytes = bytes;
                entry.persisted = true;
            }
            state.charged_bytes = total;
        }
        Ok(())
    }

    fn retain_blobs(&self, binding: &Binding) -> Result<BTreeMap<Id, Arc<MetadataFile>>, Error> {
        if binding.store_id() != &self.store_id {
            return Err(Error::IdentityMismatch);
        }
        let mut refs = BTreeMap::new();
        references(binding, &mut refs)?;
        let mut files = BTreeMap::new();
        for (id, blob) in refs {
            let file = self
                .root
                .read_private_metadata(self.scope.clone(), Path::new(&blob_name(&id)), BLOB_BYTES)
                .map_err(storage)?;
            if file.bytes().len() as u64 != blob.bytes() || digest(file.bytes()) != *blob.sha256() {
                return Err(Error::BlobChanged);
            }
            files.insert(id, Arc::new(file));
        }
        Ok(files)
    }

    pub fn plan_import<'a>(
        &'a self,
        source: &'a SourceCohort,
        base_config_digest: Digest,
    ) -> Result<PlannedImport<'a>, Error> {
        let mut payloads = Vec::new();
        let mut presences = Vec::new();
        for role in ROLES {
            let presence = if let Some(source) = source.file(role) {
                if let Some(bytes) = source.document().original_bytes() {
                    #[cfg(unix)]
                    let mode = source.permissions().ok_or(Error::SourceChanged)?;
                    #[cfg(not(unix))]
                    let mode = 0o600;
                    let sha256 =
                        Digest::new(source.sha256().ok_or(Error::SourceChanged)?.to_owned())
                            .map_err(|_| Error::SourceChanged)?;
                    let blob = Blob::new(id()?, bytes.len() as u64, sha256, mode)
                        .map_err(|_| Error::InvalidBinding)?;
                    payloads.push((blob.clone(), bytes.clone()));
                    Presence::Blob(blob)
                } else {
                    Presence::Absent
                }
            } else {
                Presence::Absent
            };
            presences.push(presence);
        }
        let aliases = if source.file(Role::Aliases).is_some() {
            Aliases::Imported {
                presence: presences.remove(6),
            }
        } else {
            Aliases::RetainedAtSource
        };
        let mut files = presences.into_iter();
        let files = Files::new(
            files.next().unwrap(),
            files.next().unwrap(),
            files.next().unwrap(),
            files.next().unwrap(),
            files.next().unwrap(),
            files.next().unwrap(),
            aliases,
        )
        .map_err(|_| Error::InvalidBinding)?;
        let transaction = id()?;
        let generation = Generation::new(transaction.clone(), files);
        let binding = Binding::new(self.store_id.clone(), generation.clone(), generation)
            .map_err(|_| Error::InvalidBinding)?;
        let intent = Intent {
            schema_version: 1,
            transaction_id: transaction,
            base_config_digest,
            candidate: binding,
        };
        Ok(PlannedImport {
            store: self,
            source,
            intent,
            payloads,
        })
    }
    pub fn prepare_import(&self, plan: PlannedImport<'_>) -> Result<PreparedBinding, Error> {
        self.prepare_import_checked(plan, |_| Ok(()))
    }
    fn prepare_import_checked(
        &self,
        plan: PlannedImport<'_>,
        mut checkpoint: impl FnMut(Point) -> Result<(), Error>,
    ) -> Result<PreparedBinding, Error> {
        if !std::ptr::eq(plan.store, self) {
            return Err(Error::IdentityMismatch);
        }
        let _preparing = self.begin()?;
        plan.source.verify().map_err(|_| Error::SourceChanged)?;
        self.verify_inventory()?;
        let verify = || plan.source.verify().map_err(|_| Error::SourceChanged);
        self.prepare(
            &verify,
            &plan.intent,
            plan.payloads,
            BTreeMap::new(),
            &mut checkpoint,
        )
    }

    pub fn plan_edit<'a>(
        &'a self,
        active: &'a PreparedBinding,
        changed: &[Document],
        base_config_digest: Digest,
    ) -> Result<PlannedEdit<'a>, Error> {
        if !Arc::ptr_eq(&self.instance, &active.store_instance)
            || active.binding.store_id() != &self.store_id
        {
            return Err(Error::IdentityMismatch);
        }
        if changed.is_empty() || changed.len() > 2 {
            return Err(Error::InvalidEdit);
        }
        let mut seen = Vec::new();
        let current = active.binding.current().files();
        let mut presences = ROLES[..6]
            .iter()
            .map(|role| current.get(*role).unwrap().clone())
            .collect::<Vec<_>>();
        let mut payloads = Vec::new();
        for document in changed {
            let role = document.role();
            if !matches!(role, Role::RecipeOptions | Role::ExecutionProfiles)
                || seen.contains(&role)
            {
                return Err(Error::InvalidEdit);
            }
            seen.push(role);
            let original = active.file(role);
            match (document.original_bytes(), original) {
                (Some(bytes), Some(file)) if bytes.as_slice() == file.bytes() => {}
                (None, None) => {}
                _ => return Err(Error::StaleDocument),
            }
            let index = ROLES
                .iter()
                .position(|candidate| *candidate == role)
                .unwrap();
            let presence = if document.is_absent() {
                Presence::Absent
            } else {
                let bytes = document.to_bytes().map_err(|_| Error::InvalidEdit)?;
                if original.is_some_and(|file| file.bytes() == bytes.as_slice()) {
                    continue;
                }
                let mode = current
                    .get(role)
                    .and_then(Presence::blob)
                    .map_or(0o600, Blob::permissions);
                let blob = Blob::new(id()?, bytes.len() as u64, digest(&bytes), mode)
                    .map_err(|_| Error::Limit)?;
                payloads.push((blob.clone(), Arc::new(bytes)));
                Presence::Blob(blob)
            };
            presences[index] = presence;
        }
        let mut next = presences.into_iter();
        let files = Files::new(
            next.next().unwrap(),
            next.next().unwrap(),
            next.next().unwrap(),
            next.next().unwrap(),
            next.next().unwrap(),
            next.next().unwrap(),
            current.aliases().clone(),
        )
        .map_err(|error| {
            if error == super::descriptor::Error::CohortTooLarge {
                Error::Limit
            } else {
                Error::InvalidBinding
            }
        })?;
        let intent = if &files == current {
            None
        } else {
            let transaction = id()?;
            let generation = Generation::new(transaction.clone(), files);
            let candidate = Binding::new(
                self.store_id.clone(),
                active.binding.original().clone(),
                generation,
            )
            .map_err(|_| Error::InvalidBinding)?;
            Some(Intent {
                schema_version: 1,
                transaction_id: transaction,
                base_config_digest: base_config_digest.clone(),
                candidate,
            })
        };
        Ok(PlannedEdit {
            store: self,
            active,
            intent,
            payloads,
            base_digest: base_config_digest,
        })
    }
    pub fn prepare_edit(&self, plan: PlannedEdit<'_>) -> Result<PreparedBinding, Error> {
        self.prepare_edit_checked(plan, |_| Ok(()))
    }
    fn prepare_edit_checked(
        &self,
        plan: PlannedEdit<'_>,
        mut checkpoint: impl FnMut(Point) -> Result<(), Error>,
    ) -> Result<PreparedBinding, Error> {
        if !std::ptr::eq(plan.store, self)
            || !Arc::ptr_eq(&self.instance, &plan.active.store_instance)
        {
            return Err(Error::IdentityMismatch);
        }
        let _preparing = self.begin()?;
        self.verify_inventory()?;
        plan.active.verify()?;
        let Some(intent) = plan.intent else {
            let identity = self
                .root
                .read_private_metadata(self.scope.clone(), Path::new(IDENTITY), METADATA_BYTES)
                .map_err(storage)?;
            let prepared = PreparedBinding {
                binding: plan.active.binding.clone(),
                identity,
                blobs: plan.active.blobs.clone(),
                store_instance: self.instance.clone(),
            };
            prepared.verify()?;
            self.identity.verify().map_err(storage)?;
            return Ok(prepared);
        };
        let mut refs = BTreeMap::new();
        references(&intent.candidate, &mut refs)?;
        let retained = plan
            .active
            .blobs
            .iter()
            .filter(|(id, _)| refs.contains_key(*id))
            .map(|(id, file)| (id.clone(), file.clone()))
            .collect();
        let verify = || plan.active.verify();
        self.prepare(&verify, &intent, plan.payloads, retained, &mut checkpoint)
    }

    fn begin(&self) -> Result<Preparing<'_>, Error> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.closed {
            return Err(Error::Closed);
        }
        if state.preparing {
            return Err(Error::Busy);
        }
        state.preparing = true;
        Ok(Preparing(self))
    }
    fn reserve(&self, intent: &Intent, payloads: &[(Blob, Arc<Vec<u8>>)]) -> Result<(), Error> {
        let mut names = vec![(intent_name(&intent.transaction_id), METADATA_BYTES)];
        let mut charge = METADATA_BYTES;
        for (blob, _) in payloads {
            add_bytes(&mut charge, blob.bytes())?;
            names.push((blob_name(blob.id()), blob.bytes()));
        }
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let charged = state
            .charged_bytes
            .checked_add(charge)
            .filter(|bytes| *bytes <= MAX_STORE_BYTES)
            .ok_or(Error::Limit)?;
        if state.intents >= MAX_INTENTS
            || state.entries.len() + names.len() > MAX_STORE_FILES
            || names
                .iter()
                .any(|(name, _)| state.entries.contains_key(name))
        {
            return Err(Error::Limit);
        }
        state.charged_bytes = charged;
        state.intents += 1;
        state.entries.extend(names.into_iter().map(|(name, bytes)| {
            (
                name,
                Entry {
                    bytes,
                    persisted: false,
                },
            )
        }));
        Ok(())
    }
    fn persisted(
        &self,
        name: &str,
        file: &MetadataFile,
        held: Option<Arc<MetadataFile>>,
    ) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let entry = state.entries.get_mut(name).ok_or(Error::InvalidStore)?;
        entry.bytes = file.bytes().len() as u64;
        entry.persisted = true;
        if let Some(held) = held {
            state.metadata.push(held);
        }
        Ok(())
    }
    fn prepare(
        &self,
        verify: &impl Fn() -> Result<(), Error>,
        intent: &Intent,
        payloads: Vec<(Blob, Arc<Vec<u8>>)>,
        mut blobs: BTreeMap<Id, Arc<MetadataFile>>,
        checkpoint: &mut impl FnMut(Point) -> Result<(), Error>,
    ) -> Result<PreparedBinding, Error> {
        let encoded = encode(intent)?;
        let cohort = intent.candidate.current().files().total_bytes();
        let space = cohort
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(SPACE_MARGIN))
            .ok_or(Error::Limit)?;
        if self
            .root
            .available_private_bytes(&self.scope)
            .map_err(storage)?
            < space
        {
            return Err(Error::Space);
        }
        verify()?;
        checkpoint(Point::BeforeIntent)?;
        verify()?;
        self.identity.verify().map_err(storage)?;
        self.reserve(intent, &payloads)?;
        let name = intent_name(&intent.transaction_id);
        let journal = Arc::new(
            self.root
                .write_new_private_metadata(self.scope.clone(), Path::new(&name), encoded)
                .map_err(storage)?,
        );
        self.persisted(&name, &journal, Some(journal.clone()))?;
        checkpoint(Point::AfterIntent)?;
        verify()?;
        self.identity.verify().map_err(storage)?;
        for (blob, bytes) in payloads {
            verify()?;
            self.identity.verify().map_err(storage)?;
            let name = blob_name(blob.id());
            let file = self
                .root
                .write_new_private_metadata(self.scope.clone(), Path::new(&name), bytes)
                .map_err(storage)?;
            self.persisted(&name, &file, None)?;
            blobs.insert(blob.id().clone(), Arc::new(file));
            checkpoint(Point::AfterBlob)?;
            verify()?;
            self.identity.verify().map_err(storage)?;
        }
        let identity = self
            .root
            .read_private_metadata(self.scope.clone(), Path::new(IDENTITY), METADATA_BYTES)
            .map_err(storage)?;
        let prepared = PreparedBinding {
            binding: intent.candidate.clone(),
            identity,
            blobs,
            store_instance: self.instance.clone(),
        };
        checkpoint(Point::BeforeReturn)?;
        verify()?;
        prepared.verify()?;
        self.identity.verify().map_err(storage)?;
        Ok(prepared)
    }
}

struct Preparing<'a>(&'a Store);
impl Drop for Preparing<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.preparing = false;
        }
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum Point {
    BeforeIntent,
    AfterIntent,
    AfterBlob,
    BeforeReturn,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::native_settings::{
        ProfileSelection, RecognizedRows, SavedOptions, SelectionContext, Settings,
    };
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        sync::atomic::AtomicBool,
    };

    struct Fixture {
        _directory: tempfile::TempDir,
        root: DirectoryLease,
        scope: Arc<LiveRoots>,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let parent = DirectoryLease::acquire(directory.path().join("managed")).unwrap();
            let initial = LiveRoots::single(parent.clone()).unwrap();
            let root = parent
                .acquire_private_child(&initial, Path::new("private"), true)
                .unwrap();
            let scope = LiveRoots::new(vec![parent, root.clone()]).unwrap();
            Self {
                _directory: directory,
                root,
                scope,
            }
        }
        fn open(&self, active: Option<&Binding>, initialize: bool) -> Result<Store, Error> {
            Store::open(self.root.clone(), self.scope.clone(), active, initialize)
        }
        // A process forked by a concurrent test holds the inherited lock descriptor until exec.
        fn reopen(&self, active: Option<&Binding>) -> Result<Store, Error> {
            for _ in 0..200 {
                match self.open(active, false) {
                    Err(Error::Busy) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    other => return other,
                }
            }
            self.open(active, false)
        }
        fn entries(&self) -> BTreeMap<String, u64> {
            self.root
                .private_metadata_entries(&self.scope, MAX_STORE_FILES)
                .unwrap()
                .into_iter()
                .map(|(name, size)| (name.into_string().unwrap(), size))
                .collect()
        }
        fn write(&self, name: &str, bytes: &[u8]) {
            self.root
                .write_new_private_metadata(
                    self.scope.clone(),
                    Path::new(name),
                    Arc::new(bytes.to_vec()),
                )
                .unwrap();
        }
    }
    fn source(files: &[(&str, &[u8])], aliases: bool) -> (tempfile::TempDir, SourceCohort) {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            fs::write(root.path().join(name), bytes).unwrap();
        }
        let source =
            SourceCohort::inspect(root.path(), aliases, Arc::new(AtomicBool::new(false))).unwrap();
        (root, source)
    }
    fn base() -> Digest {
        digest(b"canonical existing configuration")
    }
    fn settings(active: &PreparedBinding) -> Settings {
        let document = |role| {
            Document::parse(role, active.file(role).map(MetadataFile::shared_bytes)).unwrap()
        };
        Settings::from_documents(
            document(Role::RecipeOptions),
            document(Role::ExecutionProfiles),
            document(Role::UserModels),
            RecognizedRows::new(["user.llama".to_owned()], []).unwrap(),
        )
        .unwrap()
    }
    fn changed_options(active: &PreparedBinding, threads: u32) -> Document {
        settings(active)
            .merge_options(
                "user.llama",
                &SavedOptions::parse(format!("{{\"threads\":{threads}}}").as_bytes()).unwrap(),
            )
            .unwrap()
            .document(Role::RecipeOptions)
            .unwrap()
            .clone()
    }

    #[test]
    fn planning_is_write_free_and_import_retains_shared_private_bytes_modes_and_explicit_absences()
    {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let root = tempfile::tempdir().unwrap();
        let bytes = b"{ \"private\" : {\"opaque\":true} }\n";
        fs::write(root.path().join("config.json"), bytes).unwrap();
        fs::set_permissions(
            root.path().join("config.json"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        fs::write(
            root.path().join("aliases.json"),
            b"{\"retained\":\"source\"}",
        )
        .unwrap();
        let source =
            SourceCohort::inspect(root.path(), false, Arc::new(AtomicBool::new(false))).unwrap();
        let before = fixture.entries();
        let reservation = store.snapshot().unwrap();
        let plan = store.plan_import(&source, base()).unwrap();
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), reservation);
        assert_eq!(plan.base_config_digest(), &base());
        assert_eq!(plan.binding().original(), plan.binding().current());
        assert_eq!(
            plan.binding().current().files().aliases(),
            &Aliases::RetainedAtSource
        );
        assert_eq!(
            plan.binding().current().files().get(Role::Jobs),
            Some(&Presence::Absent)
        );
        let prepared = store.prepare_import(plan).unwrap();
        prepared.verify().unwrap();
        let blob = prepared
            .binding()
            .current()
            .files()
            .get(Role::Config)
            .unwrap()
            .blob()
            .unwrap();
        assert_eq!(blob.permissions(), 0o640);
        assert_eq!(prepared.file(Role::Config).unwrap().bytes(), bytes);
        assert!(Arc::ptr_eq(
            &prepared.file(Role::Config).unwrap().shared_bytes(),
            source
                .file(Role::Config)
                .unwrap()
                .document()
                .original_bytes()
                .unwrap()
        ));
        assert_eq!(
            fs::metadata(fixture.root.root().join(blob_name(blob.id())))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
        assert!(prepared.file(Role::Jobs).is_none());
        assert!(prepared.file(Role::Aliases).is_none());
        assert_eq!(fs::read(root.path().join("config.json")).unwrap(), bytes);
        assert_eq!(
            fs::metadata(root.path().join("config.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o640
        );
        let entries = fixture.entries();
        assert_eq!(entries.len(), 3);
        assert!(
            entries.keys().all(|name| name == IDENTITY
                || intent_id(name).is_some()
                || blob_id(name).is_some())
        );
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(
                fixture
                    .root
                    .root()
                    .join(intent_name(prepared.binding().current().id())),
            )
            .unwrap(),
        )
        .unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "schema_version",
                "transaction_id",
                "base_config_digest",
                "candidate"
            ])
        );
        assert_eq!(
            store.snapshot().unwrap().charged_bytes,
            2 * METADATA_BYTES + bytes.len() as u64
        );
    }

    #[test]
    fn aliases_are_imported_only_when_the_source_cohort_explicitly_owns_them() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_source, cohort) = source(&[("aliases.json", b"{\"alias\":\"target\"}")], true);
        let prepared = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        assert!(prepared.binding().current().files().aliases().is_imported());
        assert_eq!(
            prepared.file(Role::Aliases).unwrap().bytes(),
            b"{\"alias\":\"target\"}"
        );
        assert!(prepared.file(Role::Config).is_none());
    }

    #[test]
    fn failures_before_intent_leave_no_new_files_or_charges_and_after_intent_restart_reserves_missing_payloads()
     {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (root, cohort) = source(&[("config.json", b"{\"private\":1}")], false);
        let before = fixture.entries();
        let state = store.snapshot().unwrap();
        let error = store
            .prepare_import_checked(store.plan_import(&cohort, base()).unwrap(), |point| {
                if point == Point::BeforeIntent {
                    fs::write(root.path().join("config.json"), b"{\"changed\":2}").unwrap();
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert_eq!(error, Error::SourceChanged);
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), state);
        let cohort =
            SourceCohort::inspect(root.path(), false, Arc::new(AtomicBool::new(false))).unwrap();
        let error = store
            .prepare_import_checked(store.plan_import(&cohort, base()).unwrap(), |point| {
                if point == Point::AfterIntent {
                    return Err(Error::Unavailable);
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert_eq!(error, Error::Unavailable);
        let reserved = store.snapshot().unwrap();
        assert_eq!(reserved.intents, 1);
        assert!(!reserved.preparing);
        let entries = fixture.entries();
        assert_eq!(entries.len(), 2);
        assert!(!entries.keys().any(|name| blob_id(name).is_some()));
        drop(store);
        let reopened = fixture.open(None, false).unwrap();
        assert_eq!(reopened.snapshot().unwrap(), reserved);
        assert_eq!(fixture.entries(), entries);
    }

    #[test]
    fn partial_cohort_keeps_full_reservation_across_restart_without_double_charging_the_written_blob()
     {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(
            &[
                ("config.json", b"{\"one\":1}"),
                ("jobs.json", b"{\"two\":2}"),
            ],
            false,
        );
        let error = store
            .prepare_import_checked(store.plan_import(&cohort, base()).unwrap(), |point| {
                if point == Point::AfterBlob {
                    return Err(Error::Unavailable);
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert_eq!(error, Error::Unavailable);
        let reserved = store.snapshot().unwrap();
        assert_eq!(
            reserved.charged_bytes,
            2 * METADATA_BYTES + cohort.total_bytes() as u64
        );
        let entries = fixture.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .keys()
                .filter(|name| blob_id(name).is_some())
                .count(),
            1
        );
        drop(store);
        let reopened = fixture.open(None, false).unwrap();
        assert_eq!(reopened.snapshot().unwrap(), reserved);
        assert_eq!(fixture.entries(), entries);
    }

    #[test]
    fn duplicate_store_is_excluded_even_with_the_same_claimed_lease_and_dropping_owner_releases_it()
    {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        assert_eq!(fixture.open(None, false).err().unwrap(), Error::Busy);
        let (_root, cohort) = source(&[("config.json", b"{}")], false);
        let prepared = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        let binding = prepared.binding().clone();
        drop(prepared);
        drop(store);
        fixture.open(Some(&binding), false).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn identity_semantic_lock_rejects_another_pid_independently_of_directory_claim_lookup() {
        const PROBE_PATH: &str = "JUICED_NATIVE_IDENTITY_LOCK_PROBE_PATH";
        const PROBE_PARENT: &str = "JUICED_NATIVE_IDENTITY_LOCK_PROBE_PARENT";
        if let Some(path) = std::env::var_os(PROBE_PATH) {
            assert_eq!(
                unsafe { libc::getppid() }.to_string(),
                std::env::var(PROBE_PARENT).unwrap()
            );
            use std::os::unix::fs::OpenOptionsExt;
            let identity = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .unwrap();
            let result = identity.try_lock();
            assert!(matches!(result, Err(std::fs::TryLockError::WouldBlock)));
            return;
        }
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_store::generations::tests::identity_semantic_lock_rejects_another_pid_independently_of_directory_claim_lookup"])
            .env(PROBE_PATH, fixture.root.root().join(IDENTITY))
            .env(PROBE_PARENT, std::process::id().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn().unwrap();
        struct Child(std::process::Child, bool);
        impl Drop for Child {
            fn drop(&mut self) {
                if !self.1 {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
        }
        let mut child = Child(child, false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                child.1 = true;
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned identity lock probe exceeded its deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert!(
            status.success(),
            "owned identity lock probe failed: {status}"
        );
        assert_eq!(fixture.open(None, false).err().unwrap(), Error::Busy);
        let (_root, cohort) = source(&[("config.json", b"{}")], false);
        store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
    }

    #[test]
    fn caller_supplied_active_binding_reads_exact_generation_without_an_intent_or_newest_selection()
    {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_first, source_one) = source(&[("config.json", b"{\"revision\":1}")], false);
        let original = store
            .prepare_import(store.plan_import(&source_one, base()).unwrap())
            .unwrap();
        let binding = original.binding().clone();
        drop(original);
        let (_second, source_two) = source(&[("config.json", b"{\"revision\":2}")], false);
        let newer = store
            .prepare_import(store.plan_import(&source_two, base()).unwrap())
            .unwrap();
        drop(newer);
        drop(store);
        let reopened = fixture.open(Some(&binding), false).unwrap();
        let active = reopened.read_binding(&binding).unwrap();
        assert_eq!(
            active.file(Role::Config).unwrap().bytes(),
            b"{\"revision\":1}"
        );
        assert_eq!(
            active.original_file(Role::Config).unwrap().bytes(),
            b"{\"revision\":1}"
        );
        drop(active);
        drop(reopened);
        for name in fixture
            .entries()
            .keys()
            .filter(|name| intent_id(name).is_some())
        {
            fs::remove_file(fixture.root.root().join(name)).unwrap();
        }
        let reopened = fixture.open(Some(&binding), false).unwrap();
        let active = reopened.read_binding(&binding).unwrap();
        active.verify().unwrap();
        assert_eq!(
            active.file(Role::Config).unwrap().bytes(),
            b"{\"revision\":1}"
        );
        assert_eq!(reopened.snapshot().unwrap().intents, 0);
    }

    #[test]
    fn a_plan_cannot_cross_store_identity_even_if_no_effects_have_started() {
        let first = Fixture::new();
        let first_store = first.open(None, true).unwrap();
        let second = Fixture::new();
        let second_store = second.open(None, true).unwrap();
        let (_root, cohort) = source(&[("config.json", b"{}")], false);
        let before = second.entries();
        let state = second_store.snapshot().unwrap();
        assert_eq!(
            second_store
                .prepare_import(first_store.plan_import(&cohort, base()).unwrap())
                .err()
                .unwrap(),
            Error::IdentityMismatch
        );
        assert_eq!(second.entries(), before);
        assert_eq!(second_store.snapshot().unwrap(), state);
    }

    #[test]
    fn overlapping_preparations_are_rejected_before_io_or_a_second_reservation() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(&[("config.json", b"{}")], false);
        store
            .prepare_import_checked(store.plan_import(&cohort, base()).unwrap(), |point| {
                if point == Point::AfterIntent {
                    let before = fixture.entries();
                    let reservation = store.snapshot().unwrap();
                    assert!(reservation.preparing);
                    assert_eq!(
                        store
                            .prepare_import(store.plan_import(&cohort, base()).unwrap())
                            .err()
                            .unwrap(),
                        Error::Busy
                    );
                    assert_eq!(store.snapshot().unwrap(), reservation);
                    assert_eq!(fixture.entries(), before);
                }
                Ok(())
            })
            .unwrap();
        assert!(!store.snapshot().unwrap().preparing);
    }

    #[test]
    fn existing_only_or_nonempty_initialization_never_invents_an_identity() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.open(None, false).err().unwrap(),
            Error::IdentityMissing
        );
        assert!(fixture.entries().is_empty());
        fixture.write(&format!("blob-{:032x}", 7), b"orphan");
        let before = fixture.entries();
        assert_eq!(
            fixture.open(None, true).err().unwrap(),
            Error::IdentityMissing
        );
        assert_eq!(fixture.entries(), before);
    }

    #[test]
    fn exact_active_binding_checks_store_identity_missing_bytes_hash_replacement_and_held_handles()
    {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(&[("config.json", b"{\"private\":1}")], false);
        let prepared = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        let binding = prepared.binding().clone();
        let name = blob_name(
            binding
                .current()
                .files()
                .get(Role::Config)
                .unwrap()
                .blob()
                .unwrap()
                .id(),
        );
        let path = fixture.root.root().join(&name);
        fs::write(&path, b"{\"private\":2}").unwrap();
        assert_eq!(prepared.verify().err().unwrap(), Error::BlobChanged);
        drop(prepared);
        drop(store);
        let before = fixture.entries();
        assert_eq!(
            fixture.reopen(Some(&binding)).err().unwrap(),
            Error::BlobChanged
        );
        assert_eq!(fixture.entries(), before);
        fs::remove_file(path).unwrap();
        assert!(fixture.open(Some(&binding), false).is_err());
        let other = Fixture::new();
        drop(other.open(None, true).unwrap());
        assert_eq!(
            other.reopen(Some(&binding)).err().unwrap(),
            Error::IdentityMismatch
        );
    }

    #[test]
    fn identity_replacement_and_unknown_names_refuse_mutation_without_repair_or_cleanup() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let replacement = fixture.root.root().join("replacement");
        fs::write(&replacement, store.identity.bytes()).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(replacement, fixture.root.root().join(IDENTITY)).unwrap();
        let (_root, cohort) = source(&[("config.json", b"{}")], false);
        let before = fixture.entries();
        let state = store.snapshot().unwrap();
        assert!(
            store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .is_err()
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), state);
        drop(store);
        fixture.write("operator-notes", b"PRIVATE_UNKNOWN_CONTENT");
        let before = fixture.entries();
        let error = fixture.open(None, false).err().unwrap();
        assert_eq!(error, Error::InvalidStore);
        assert!(!format!("{error:?} {error}").contains("PRIVATE_UNKNOWN_CONTENT"));
        assert_eq!(fixture.entries(), before);
    }

    #[test]
    fn unknown_orphan_blobs_are_conservatively_charged_and_never_deleted() {
        let fixture = Fixture::new();
        drop(fixture.open(None, true).unwrap());
        fixture.write(&format!("blob-{:032x}", 9), b"small unknown orphan");
        let before = fixture.entries();
        let store = fixture.open(None, false).unwrap();
        assert_eq!(
            store.snapshot().unwrap().charged_bytes,
            METADATA_BYTES + BLOB_BYTES
        );
        assert_eq!(fixture.entries(), before);
        drop(store);
        assert_eq!(
            fixture
                .open(None, false)
                .unwrap()
                .snapshot()
                .unwrap()
                .charged_bytes,
            METADATA_BYTES + BLOB_BYTES
        );
    }

    #[test]
    fn intent_count_and_physical_budget_refuse_before_new_intent_or_payload_effects() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(&[], false);
        for _ in 0..MAX_INTENTS {
            store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .unwrap();
        }
        let before = fixture.entries();
        let state = store.snapshot().unwrap();
        assert_eq!(
            store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .err()
                .unwrap(),
            Error::Limit
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), state);
        drop(store);
        assert_eq!(
            fixture.open(None, false).unwrap().snapshot().unwrap(),
            state
        );
        let fixture = Fixture::new();
        drop(fixture.open(None, true).unwrap());
        for index in 0..15 {
            fixture.write(&format!("blob-{index:032x}"), b"o");
        }
        let store = fixture.open(None, false).unwrap();
        let mut bytes = vec![b' '; BLOB_BYTES as usize];
        bytes[..2].copy_from_slice(b"{}");
        let (_root, cohort) = source(&[("config.json", &bytes)], false);
        let before = fixture.entries();
        let state = store.snapshot().unwrap();
        assert_eq!(
            store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .err()
                .unwrap(),
            Error::Limit
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), state);
    }

    #[test]
    fn private_permissions_symlinks_and_inventory_overflow_are_preserved_rejections() {
        let fixture = Fixture::new();
        drop(fixture.open(None, true).unwrap());
        let path = fixture.root.root().join(IDENTITY);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fixture.open(None, false).is_err());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o7777,
            0o644
        );
        let fixture = Fixture::new();
        drop(fixture.open(None, true).unwrap());
        let outside = fixture._directory.path().join("private-source");
        fs::write(&outside, b"PRIVATE_SOURCE_BYTES").unwrap();
        symlink(
            &outside,
            fixture.root.root().join(format!("blob-{:032x}", 8)),
        )
        .unwrap();
        assert!(fixture.open(None, false).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"PRIVATE_SOURCE_BYTES");
        let fixture = Fixture::new();
        drop(fixture.open(None, true).unwrap());
        for index in 0..MAX_STORE_FILES {
            fixture.write(&format!("blob-{index:032x}"), b"x");
        }
        assert!(fixture.open(None, false).is_err());
        assert_eq!(
            fs::read_dir(fixture.root.root()).unwrap().count(),
            MAX_STORE_FILES + 2
        );
    }

    #[test]
    fn root_loss_after_durable_intent_retains_charge_and_never_writes_replacement_directory() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(&[("config.json", b"{}")], false);
        let moved = fixture._directory.path().join("displaced-private");
        let error = store
            .prepare_import_checked(store.plan_import(&cohort, base()).unwrap(), |point| {
                if point == Point::AfterIntent {
                    fs::rename(fixture.root.root(), &moved).unwrap();
                    fs::create_dir(fixture.root.root()).unwrap();
                    fs::set_permissions(fixture.root.root(), fs::Permissions::from_mode(0o700))
                        .unwrap();
                    fs::write(fixture.root.root().join("foreign-marker"), b"replacement").unwrap();
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert_eq!(error, Error::RootUnavailable);
        assert_eq!(error.code(), "storage_root_unavailable");
        assert_eq!(
            store.snapshot().unwrap().charged_bytes,
            2 * METADATA_BYTES + 2
        );
        assert_eq!(fs::read_dir(fixture.root.root()).unwrap().count(), 1);
        assert_eq!(
            fs::read(fixture.root.root().join("foreign-marker")).unwrap(),
            b"replacement"
        );
        assert_eq!(fs::read_dir(moved).unwrap().count(), 3);
    }

    #[test]
    fn edits_reuse_unchanged_large_handles_preserve_original_permissions_and_never_choose_a_head() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let large =
            serde_json::to_vec(&serde_json::json!({"opaque":"x".repeat(512*1024)})).unwrap();
        let (root, _) = source(
            &[
                ("config.json", b"{\"private\":true}"),
                ("recipe_options.json", b"{\"user.llama\":{\"threads\":1}}"),
                ("user_models.json", &large),
            ],
            false,
        );
        fs::set_permissions(
            root.path().join("recipe_options.json"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let cohort =
            SourceCohort::inspect(root.path(), false, Arc::new(AtomicBool::new(false))).unwrap();
        let active = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        let before = fixture.entries();
        let reservation = store.snapshot().unwrap();
        let changed = settings(&active)
            .merge_options(
                "user.llama",
                &SavedOptions::parse(b"{\"threads\":2}").unwrap(),
            )
            .unwrap();
        let selected = changed
            .select_profile(
                "user.llama",
                SelectionContext {
                    current_recipe: "llamacpp",
                    effective_options: &SavedOptions::parse(b"{}").unwrap(),
                },
                &ProfileSelection::parse(b"{\"id\":\"registered\"}").unwrap(),
            )
            .unwrap();
        let documents = [
            selected.document(Role::RecipeOptions).unwrap().clone(),
            selected.document(Role::ExecutionProfiles).unwrap().clone(),
        ];
        let plan = store.plan_edit(&active, &documents, base()).unwrap();
        assert!(plan.is_changed());
        assert_eq!(plan.base_config_digest(), &base());
        assert_eq!(plan.binding().original(), active.binding().original());
        assert_eq!(
            plan.binding().current().files().get(Role::UserModels),
            active.binding().current().files().get(Role::UserModels)
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), reservation);
        let edited = store.prepare_edit(plan).unwrap();
        edited.verify().unwrap();
        assert_eq!(edited.binding().original(), active.binding().original());
        assert_ne!(
            edited.binding().current().id(),
            active.binding().current().id()
        );
        let unchanged = active
            .binding()
            .current()
            .files()
            .get(Role::UserModels)
            .unwrap()
            .blob()
            .unwrap()
            .id();
        assert!(Arc::ptr_eq(
            &active.blobs[unchanged],
            &edited.blobs[unchanged]
        ));
        assert!(Arc::ptr_eq(
            &active.file(Role::UserModels).unwrap().shared_bytes(),
            &edited.file(Role::UserModels).unwrap().shared_bytes()
        ));
        assert_eq!(
            edited.original_file(Role::RecipeOptions).unwrap().bytes(),
            b"{\"user.llama\":{\"threads\":1}}"
        );
        assert_eq!(
            edited.file(Role::RecipeOptions).unwrap().bytes(),
            documents[0].to_bytes().unwrap()
        );
        assert_eq!(
            edited
                .binding()
                .current()
                .files()
                .get(Role::RecipeOptions)
                .unwrap()
                .blob()
                .unwrap()
                .permissions(),
            0o640
        );
        assert_eq!(
            fs::read(root.path().join("recipe_options.json")).unwrap(),
            b"{\"user.llama\":{\"threads\":1}}"
        );
        assert_eq!(fixture.entries().len(), before.len() + 3);
        let prior = store.read_binding(active.binding()).unwrap();
        assert_eq!(
            prior.file(Role::RecipeOptions).unwrap().bytes(),
            active.file(Role::RecipeOptions).unwrap().bytes()
        );
        let binding = edited.binding().clone();
        drop(prior);
        drop(edited);
        drop(active);
        drop(store);
        let (_store, active) = Store::open_bound(
            fixture.root.clone(),
            fixture.scope.clone(),
            Some(&binding),
            false,
        )
        .unwrap();
        let active = active.unwrap();
        assert_eq!(active.binding(), &binding);
        active.verify().unwrap();
        assert_eq!(
            active.original_file(Role::RecipeOptions).unwrap().bytes(),
            b"{\"user.llama\":{\"threads\":1}}"
        );
    }

    #[test]
    fn byte_identical_dirty_edits_keep_generation_handles_files_and_charge_unchanged() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(
            &[("recipe_options.json", b"{\"user.llama\":{\"threads\":1}}")],
            false,
        );
        let active = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        let unchanged = changed_options(&active, 1);
        let entries = fixture.entries();
        let reservation = store.snapshot().unwrap();
        let plan = store.plan_edit(&active, &[unchanged], base()).unwrap();
        assert!(!plan.is_changed());
        assert_eq!(plan.binding(), active.binding());
        let repeated = store.prepare_edit(plan).unwrap();
        assert_eq!(repeated.binding(), active.binding());
        let reference = active
            .binding()
            .current()
            .files()
            .get(Role::RecipeOptions)
            .unwrap()
            .blob()
            .unwrap()
            .id();
        assert!(Arc::ptr_eq(
            &active.blobs[reference],
            &repeated.blobs[reference]
        ));
        assert_eq!(fixture.entries(), entries);
        assert_eq!(store.snapshot().unwrap(), reservation);
    }

    #[test]
    fn active_closure_verification_does_not_treat_creation_intent_as_execution_authority() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(&[("config.json", b"{\"private\":true}")], false);
        let prepared = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        let before = store.snapshot().unwrap();
        fs::remove_file(
            fixture
                .root
                .root()
                .join(intent_name(prepared.binding().current().id())),
        )
        .unwrap();
        prepared.verify().unwrap();
        assert_eq!(
            prepared.file(Role::Config).unwrap().bytes(),
            b"{\"private\":true}"
        );
        assert_eq!(
            store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .err()
                .unwrap(),
            Error::Unavailable
        );
        assert_eq!(store.snapshot().unwrap(), before);
    }

    #[test]
    fn edits_refuse_wrong_roles_duplicates_empty_requests_and_stale_or_foreign_provenance_without_effects()
     {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let (_root, cohort) = source(
            &[("recipe_options.json", b"{\"user.llama\":{\"threads\":1}}")],
            false,
        );
        let active = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        let changed = changed_options(&active, 2);
        let before = fixture.entries();
        let reservation = store.snapshot().unwrap();
        assert_eq!(
            store.plan_edit(&active, &[], base()).err().unwrap(),
            Error::InvalidEdit
        );
        assert_eq!(
            store
                .plan_edit(&active, &[changed.clone(), changed.clone()], base())
                .err()
                .unwrap(),
            Error::InvalidEdit
        );
        assert_eq!(
            store
                .plan_edit(
                    &active,
                    &[Document::parse(Role::Config, None).unwrap()],
                    base()
                )
                .err()
                .unwrap(),
            Error::InvalidEdit
        );
        let wrong = Document::parse_bytes(Role::RecipeOptions, Some(b"{\"other\":{}}")).unwrap();
        assert_eq!(
            store.plan_edit(&active, &[wrong], base()).err().unwrap(),
            Error::StaleDocument
        );
        let other = Fixture::new();
        let other_store = other.open(None, true).unwrap();
        assert_eq!(
            other_store
                .plan_edit(&active, std::slice::from_ref(&changed), base())
                .err()
                .unwrap(),
            Error::IdentityMismatch
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), reservation);
        let next = store
            .prepare_edit(
                store
                    .plan_edit(&active, std::slice::from_ref(&changed), base())
                    .unwrap(),
            )
            .unwrap();
        let before = fixture.entries();
        let reservation = store.snapshot().unwrap();
        assert_eq!(
            store.plan_edit(&next, &[changed], base()).err().unwrap(),
            Error::StaleDocument
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), reservation);
    }

    #[test]
    fn incomplete_hash_damaged_and_grown_pending_edits_keep_old_active_binding_readable_and_full_charges()
     {
        for damage in 0..3 {
            let fixture = Fixture::new();
            let store = fixture.open(None, true).unwrap();
            let (_root, cohort) = source(
                &[("recipe_options.json", b"{\"user.llama\":{\"threads\":1}}")],
                false,
            );
            let active = store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .unwrap();
            let old = active.binding().clone();
            let before = store.snapshot().unwrap();
            let changed = changed_options(&active, 2);
            let plan = store.plan_edit(&active, &[changed], base()).unwrap();
            let candidate = plan.binding().clone();
            let blob = candidate
                .current()
                .files()
                .get(Role::RecipeOptions)
                .unwrap()
                .blob()
                .unwrap();
            let name = blob_name(blob.id());
            let damaged = vec![
                b'z';
                match damage {
                    0 => 3,
                    1 => blob.bytes() as usize,
                    _ => blob.bytes() as usize + 1,
                }
            ];
            assert_eq!(
                store
                    .prepare_edit_checked(plan, |point| {
                        if point == Point::AfterIntent {
                            fixture.write(&name, &damaged);
                            return Err(Error::Unavailable);
                        }
                        Ok(())
                    })
                    .err()
                    .unwrap(),
                Error::Unavailable
            );
            let retained = store.read_binding(&old).unwrap();
            retained.verify().unwrap();
            assert_eq!(
                retained.file(Role::RecipeOptions).unwrap().bytes(),
                b"{\"user.llama\":{\"threads\":1}}"
            );
            assert_eq!(
                store.read_binding(&candidate).err().unwrap(),
                Error::BlobChanged
            );
            let reservation = store.snapshot().unwrap();
            assert_eq!(
                reservation.charged_bytes,
                before.charged_bytes
                    + METADATA_BYTES
                    + if damage == 2 {
                        BLOB_BYTES
                    } else {
                        blob.bytes()
                    }
            );
            let entries = fixture.entries();
            assert_eq!(entries[&name], damaged.len() as u64);
            drop(retained);
            drop(active);
            drop(store);
            let (reopened, retained) = Store::open_bound(
                fixture.root.clone(),
                fixture.scope.clone(),
                Some(&old),
                false,
            )
            .unwrap();
            let retained = retained.unwrap();
            retained.verify().unwrap();
            assert_eq!(
                retained.file(Role::RecipeOptions).unwrap().bytes(),
                b"{\"user.llama\":{\"threads\":1}}"
            );
            assert_eq!(reopened.snapshot().unwrap(), reservation);
            assert_eq!(
                reopened.read_binding(&candidate).err().unwrap(),
                Error::BlobChanged
            );
            assert_eq!(fixture.entries(), entries);
            assert_eq!(fs::read(fixture.root.root().join(name)).unwrap(), damaged);
        }
    }

    #[test]
    fn edit_logical_cohort_limit_rejects_candidate_before_any_intent_or_reservation() {
        let fixture = Fixture::new();
        let store = fixture.open(None, true).unwrap();
        let mut bytes = vec![b' '; BLOB_BYTES as usize];
        bytes[..2].copy_from_slice(b"{}");
        let (_root, cohort) = source(
            &[
                ("config.json", &bytes),
                ("jobs.json", &bytes),
                ("user_models.json", &bytes),
                ("native_helpers.json", &bytes),
            ],
            false,
        );
        let active = store
            .prepare_import(store.plan_import(&cohort, base()).unwrap())
            .unwrap();
        assert_eq!(
            active.binding().current().files().total_bytes(),
            super::super::descriptor::MAX_GENERATION_BYTES
        );
        let changed = changed_options(&active, 2);
        let before = fixture.entries();
        let reservation = store.snapshot().unwrap();
        assert_eq!(
            store.plan_edit(&active, &[changed], base()).err().unwrap(),
            Error::Limit
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(store.snapshot().unwrap(), reservation);
    }

    #[test]
    fn closing_rejects_new_operations_but_retains_owner_and_admitted_writer_until_io_settles() {
        struct Writer {
            release: Option<std::sync::mpsc::Sender<()>>,
            thread: Option<std::thread::JoinHandle<Result<PreparedBinding, Error>>>,
        }
        impl Writer {
            fn finish(mut self) -> Result<PreparedBinding, Error> {
                self.release.take().unwrap().send(()).unwrap();
                self.thread.take().unwrap().join().unwrap()
            }
        }
        impl Drop for Writer {
            fn drop(&mut self) {
                if let Some(release) = self.release.take() {
                    let _ = release.send(());
                }
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
            }
        }
        for held in [Point::BeforeIntent, Point::AfterIntent] {
            let fixture = Fixture::new();
            let store = Arc::new(fixture.open(None, true).unwrap());
            let (_original, cohort) = source(&[("config.json", b"{}")], false);
            let active = store
                .prepare_import(store.plan_import(&cohort, base()).unwrap())
                .unwrap();
            let (_next, cohort) = source(&[("config.json", b"{\"next\":true}")], false);
            let cohort = Arc::new(cohort);
            let (entered, observed) = std::sync::mpsc::sync_channel(0);
            let (release, wait_release) = std::sync::mpsc::channel();
            let owner = store.clone();
            let source = cohort.clone();
            let thread = std::thread::spawn(move || {
                owner.prepare_import_checked(owner.plan_import(&source, base()).unwrap(), |point| {
                    if point == held {
                        entered.send(()).unwrap();
                        wait_release
                            .recv_timeout(std::time::Duration::from_secs(2))
                            .unwrap();
                    }
                    Ok(())
                })
            });
            let writer = Writer {
                release: Some(release),
                thread: Some(thread),
            };
            observed
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            let before = store.snapshot().unwrap();
            assert!(before.preparing);
            assert!(!before.closed);
            let files = fixture.entries();
            store.close().unwrap();
            store.close().unwrap();
            let closing = store.snapshot().unwrap();
            assert!(closing.closed);
            assert!(closing.preparing);
            assert_eq!(closing.charged_bytes, before.charged_bytes);
            assert_eq!(closing.reserved_files, before.reserved_files);
            assert_eq!(closing.intents, before.intents);
            assert_eq!(fixture.entries(), files);
            assert_eq!(fixture.open(None, false).err().unwrap(), Error::Busy);
            assert_eq!(
                store
                    .prepare_import(store.plan_import(&cohort, base()).unwrap())
                    .err()
                    .unwrap(),
                Error::Closed
            );
            let changed = changed_options(&active, 2);
            assert_eq!(
                store
                    .prepare_edit(store.plan_edit(&active, &[changed], base()).unwrap())
                    .err()
                    .unwrap(),
                Error::Closed
            );
            assert_eq!(
                store.read_binding(active.binding()).err().unwrap(),
                Error::Closed
            );
            assert_eq!(store.snapshot().unwrap(), closing);
            active.verify().unwrap();
            let prepared = writer.finish().unwrap();
            prepared.verify().unwrap();
            active.verify().unwrap();
            let settled = store.snapshot().unwrap();
            assert!(settled.closed);
            assert!(!settled.preparing);
            assert_eq!(settled.intents, 2);
            assert_eq!(fixture.entries().len(), 5);
            assert_eq!(active.file(Role::Config).unwrap().bytes(), b"{}");
            assert_eq!(
                prepared.file(Role::Config).unwrap().bytes(),
                b"{\"next\":true}"
            );
            assert_eq!(fixture.open(None, false).err().unwrap(), Error::Busy);
            assert_eq!(
                store.read_binding(prepared.binding()).err().unwrap().code(),
                "shutting_down"
            );
        }
    }

    #[test]
    fn error_diagnostics_include_only_static_code_reason_and_preservation_guidance() {
        for error in [
            Error::Closed,
            Error::RootUnavailable,
            Error::InvalidIntent,
            Error::StaleDocument,
            Error::Limit,
        ] {
            let display = error.to_string();
            let debug = format!("{error:?}");
            for output in [&display, &debug] {
                assert!(output.contains(error.code()));
                assert!(output.contains(error.reason()));
                assert!(output.contains("Preserve the source and settings store."));
                assert!(!output.contains("PRIVATE_METADATA_PAYLOAD"));
            }
            assert!(std::error::Error::source(&error).is_none());
        }
    }
}
