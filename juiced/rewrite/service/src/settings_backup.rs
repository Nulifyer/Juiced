use crate::{
    config::{Config, ConfigurationBinding, MAXIMUM_CONFIG_BYTES},
    native_store::{
        descriptor::STORE_ROOT,
        generations::{MAX_STORE_BYTES, MAX_STORE_FILES, Store, metadata_entry_limit},
    },
    storage::{
        DirectoryLease, DirectoryReader, LiveRoots, MetadataCopy, MetadataFile, MetadataObservation,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const CONFIGURATION: &str = "configuration.json";
const MANIFEST: &str = "backup.json";
const INTENT: &str = "backup-intent.json";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_CAPSULE_FILES: usize = MAX_STORE_FILES + 3;

#[derive(Clone, Copy)]
pub struct Error {
    reason: &'static str,
}
impl Error {
    fn new(reason: &'static str) -> Self {
        Self { reason }
    }
    pub fn code(self) -> &'static str {
        "settings_backup_recovery_required"
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: reason={}; Preserve the settings source and incomplete destination.",
            self.code(),
            self.reason
        )
    }
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsBackupError")
            .field("code", &self.code())
            .field("reason", &self.reason)
            .finish()
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;
fn storage(error: std::io::Error) -> Error {
    if crate::storage::is_root_unavailable(&error) {
        return Error::new("storage_root_unavailable");
    }
    Error::new(match error.kind() {
        std::io::ErrorKind::WouldBlock => "owner_busy",
        std::io::ErrorKind::AlreadyExists => "destination_conflict",
        std::io::ErrorKind::Unsupported => "platform_unsupported",
        _ => "storage_unavailable",
    })
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    bytes: u64,
    sha256: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    scope: String,
    configuration: Entry,
    settings: Vec<Entry>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema_version: u32,
    configuration_sha256: String,
    settings: Vec<(String, u64)>,
}
#[derive(Debug, Serialize)]
pub struct Receipt {
    pub schema_version: u32,
    pub scope: &'static str,
    pub settings_files: usize,
    pub bytes: u64,
    pub manifest_sha256: String,
    pub complete: bool,
}

fn check(deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::new("cancelled"));
    }
    if Instant::now() >= deadline {
        return Err(Error::new("deadline"));
    }
    Ok(())
}
fn encode(value: &impl Serialize) -> Result<Arc<Vec<u8>>> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::new("invalid_manifest"))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(Error::new("manifest_limit"));
    }
    Ok(Arc::new(bytes))
}
fn private_child(
    parent: &DirectoryLease,
    scope: &LiveRoots,
    path: &Path,
    create: bool,
) -> Result<DirectoryLease> {
    parent
        .acquire_private_child(scope, path, create)
        .map_err(storage)
}

pub fn export(
    configuration: PathBuf,
    destination: PathBuf,
    cancelled: Arc<AtomicBool>,
) -> Result<Receipt> {
    export_before(
        configuration,
        destination,
        cancelled,
        Instant::now() + Duration::from_secs(30),
    )
}
fn export_before(
    configuration: PathBuf,
    destination: PathBuf,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Receipt> {
    check(deadline, &cancelled)?;
    let configuration_path = std::path::absolute(configuration).map_err(storage)?;
    let destination = std::path::absolute(destination).map_err(storage)?;
    let source_parent = std::fs::canonicalize(
        configuration_path
            .parent()
            .ok_or_else(|| Error::new("configuration_unavailable"))?,
    )
    .map_err(storage)?;
    let target_parent = std::fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| Error::new("destination_invalid"))?,
    )
    .map_err(storage)?;
    if target_parent.starts_with(source_parent.join(STORE_ROOT)) {
        return Err(Error::new("destination_overlaps_settings"));
    }
    let roots = DirectoryLease::acquire_many([source_parent.clone(), target_parent.clone()])
        .map_err(storage)?;
    let root = roots
        .iter()
        .find(|root| root.root() == source_parent)
        .ok_or_else(|| Error::new("configuration_unavailable"))?
        .clone();
    let parent = roots
        .iter()
        .find(|root| root.root() == target_parent)
        .ok_or_else(|| Error::new("destination_invalid"))?
        .clone();
    let configuration = ConfigurationBinding::from_root(
        root,
        Path::new(
            configuration_path
                .file_name()
                .ok_or_else(|| Error::new("configuration_unavailable"))?,
        ),
    )
    .map_err(|_| Error::new("configuration_unavailable"))?;
    let reader = DirectoryReader::open_existing(configuration.root.root()).map_err(storage)?;
    let observed = reader
        .observe(configuration.filename(), MAXIMUM_CONFIG_BYTES)
        .map_err(storage)?;
    let bytes = observed
        .shared_bytes()
        .ok_or_else(|| Error::new("configuration_absent"))?;
    let config: Config =
        serde_json::from_slice(&bytes).map_err(|_| Error::new("configuration_invalid"))?;
    config
        .validate()
        .map_err(|_| Error::new("configuration_invalid"))?;
    let binding = config
        .native_settings
        .as_ref()
        .ok_or_else(|| Error::new("native_settings_owner_required"))?;
    let initial = LiveRoots::single(configuration.root.clone()).map_err(storage)?;
    let native = private_child(&configuration.root, &initial, Path::new(STORE_ROOT), false)?;
    let source_scope =
        LiveRoots::new(vec![configuration.root.clone(), native.clone()]).map_err(storage)?;
    let (store, active) =
        Store::open_bound(native.clone(), source_scope.clone(), Some(binding), false)
            .map_err(|_| Error::new("settings_invalid"))?;
    let active = active.ok_or_else(|| Error::new("settings_invalid"))?;
    active
        .verify()
        .map_err(|_| Error::new("settings_changed"))?;
    let mut source_files = native
        .private_metadata_entries(&source_scope, MAX_STORE_FILES)
        .map_err(storage)?;
    source_files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut planned = Vec::with_capacity(source_files.len());
    let mut total = bytes.len() as u64;
    for (name, count) in &source_files {
        let name = name
            .to_str()
            .ok_or_else(|| Error::new("settings_invalid"))?;
        if *count > metadata_entry_limit(name).ok_or_else(|| Error::new("settings_invalid"))? {
            return Err(Error::new("settings_limit"));
        }
        total = total
            .checked_add(*count)
            .filter(|total| *total <= MAX_STORE_BYTES + MAXIMUM_CONFIG_BYTES)
            .ok_or_else(|| Error::new("settings_limit"))?;
        planned.push((name.to_owned(), *count));
    }
    observed.verify().map_err(storage)?;
    check(deadline, &cancelled)?;
    let parent_scope = LiveRoots::new(vec![
        configuration.root.clone(),
        native.clone(),
        parent.clone(),
    ])
    .map_err(storage)?;
    let output = private_child(
        &parent,
        &parent_scope,
        Path::new(
            destination
                .file_name()
                .ok_or_else(|| Error::new("destination_invalid"))?,
        ),
        true,
    )?;
    let scope = LiveRoots::new(vec![
        configuration.root.clone(),
        native.clone(),
        parent,
        output.clone(),
    ])
    .map_err(storage)?;
    if !output
        .private_entry_names(&scope, MAX_CAPSULE_FILES)
        .map_err(storage)?
        .is_empty()
    {
        return Err(Error::new("destination_occupied"));
    }
    let required = total
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(1024 * 1024))
        .ok_or_else(|| Error::new("settings_limit"))?;
    if output.available_private_bytes(&scope).map_err(storage)? < required {
        return Err(Error::new("space_unavailable"));
    }
    let config_entry = Entry {
        name: CONFIGURATION.to_owned(),
        bytes: bytes.len() as u64,
        sha256: hash(&bytes),
    };
    let intent = output
        .write_new_private_metadata(
            scope.clone(),
            Path::new(INTENT),
            encode(&Intent {
                schema_version: 1,
                configuration_sha256: config_entry.sha256.clone(),
                settings: planned.clone(),
            })?,
        )
        .map_err(storage)?;
    intent
        .verify_digest(&hash(intent.bytes()))
        .map_err(storage)?;
    check(deadline, &cancelled)?;
    observed.verify().map_err(storage)?;
    let saved_config = output
        .write_new_private_metadata(scope.clone(), Path::new(CONFIGURATION), bytes)
        .map_err(storage)?;
    saved_config
        .verify_digest(&config_entry.sha256)
        .map_err(storage)?;
    let mut copies: Vec<MetadataCopy> = Vec::with_capacity(planned.len());
    let mut settings = Vec::with_capacity(planned.len());
    for (name, count) in planned {
        check(deadline, &cancelled)?;
        observed.verify().map_err(storage)?;
        let copy = native
            .copy_new_private_metadata_to(
                scope.clone(),
                Path::new(&name),
                &output,
                Path::new(&name),
                count,
            )
            .map_err(storage)?;
        if copy.bytes() != count {
            return Err(Error::new("settings_changed"));
        }
        settings.push(Entry {
            name,
            bytes: count,
            sha256: copy.sha256().to_owned(),
        });
        copies.push(copy);
    }
    let verify = || -> Result<()> {
        check(deadline, &cancelled)?;
        observed.verify().map_err(storage)?;
        intent.verify().map_err(storage)?;
        saved_config.verify().map_err(storage)?;
        active
            .verify()
            .map_err(|_| Error::new("settings_changed"))?;
        store
            .verify_retained()
            .map_err(|_| Error::new("settings_changed"))?;
        for copy in &copies {
            copy.verify().map_err(storage)?;
        }
        let mut current = native
            .private_metadata_entries(&scope, MAX_STORE_FILES)
            .map_err(storage)?;
        current.sort_by(|a, b| a.0.cmp(&b.0));
        if current != source_files {
            return Err(Error::new("settings_changed"));
        }
        Ok(())
    };
    verify()?;
    let mut expected_names = source_files
        .iter()
        .map(|(name, _)| name.clone())
        .chain([INTENT.into(), CONFIGURATION.into()])
        .collect::<BTreeSet<_>>();
    if output
        .private_entry_names(&scope, MAX_CAPSULE_FILES)
        .map_err(storage)?
        .into_iter()
        .collect::<BTreeSet<_>>()
        != expected_names
    {
        return Err(Error::new("destination_changed"));
    }
    let manifest = encode(&Manifest {
        schema_version: 1,
        scope: "configuration-and-imported-native-settings".to_owned(),
        configuration: config_entry,
        settings,
    })?;
    let manifest_sha256 = hash(&manifest);
    let committed = output
        .write_new_private_metadata(scope.clone(), Path::new(MANIFEST), manifest)
        .map_err(storage)?;
    verify()?;
    committed.verify().map_err(storage)?;
    committed.verify_digest(&manifest_sha256).map_err(storage)?;
    expected_names.insert(MANIFEST.into());
    if output
        .private_entry_names(&scope, MAX_CAPSULE_FILES)
        .map_err(storage)?
        .into_iter()
        .collect::<BTreeSet<_>>()
        != expected_names
    {
        return Err(Error::new("destination_changed"));
    }
    Ok(Receipt {
        schema_version: 1,
        scope: "Exact configuration and complete retained imported-settings store; excludes model artifacts, engine packages and computation cache",
        settings_files: source_files.len(),
        bytes: total,
        manifest_sha256,
        complete: true,
    })
}

struct Capsule {
    root: DirectoryLease,
    scope: Arc<LiveRoots>,
    manifest: MetadataFile,
    intent: MetadataFile,
    configuration: MetadataFile,
    config: Config,
    entries: Vec<Entry>,
    observed: Vec<MetadataObservation>,
}
impl Capsule {
    fn open(
        root: DirectoryLease,
        scope: Arc<LiveRoots>,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self> {
        check(deadline, cancelled)?;
        let manifest = root
            .read_private_metadata(scope.clone(), Path::new(MANIFEST), MAX_MANIFEST_BYTES)
            .map_err(storage)?;
        let decoded: Manifest =
            serde_json::from_slice(manifest.bytes()).map_err(|_| Error::new("manifest_invalid"))?;
        if decoded.schema_version != 1
            || decoded.scope != "configuration-and-imported-native-settings"
            || decoded.configuration.name != CONFIGURATION
            || decoded.configuration.bytes > MAXIMUM_CONFIG_BYTES
            || decoded.settings.is_empty()
            || decoded.settings.len() > MAX_STORE_FILES
        {
            return Err(Error::new("manifest_invalid"));
        }
        let digest_valid = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if !digest_valid(&decoded.configuration.sha256) {
            return Err(Error::new("manifest_invalid"));
        }
        let mut names = BTreeSet::new();
        let mut total = decoded.configuration.bytes;
        let mut previous: Option<&str> = None;
        for entry in &decoded.settings {
            let limit =
                metadata_entry_limit(&entry.name).ok_or_else(|| Error::new("manifest_invalid"))?;
            if entry.bytes > limit
                || !digest_valid(&entry.sha256)
                || previous.is_some_and(|name| name >= entry.name.as_str())
            {
                return Err(Error::new("manifest_invalid"));
            }
            total = total
                .checked_add(entry.bytes)
                .filter(|bytes| *bytes <= MAX_STORE_BYTES + MAXIMUM_CONFIG_BYTES)
                .ok_or_else(|| Error::new("manifest_limit"))?;
            previous = Some(&entry.name);
            names.insert(std::ffi::OsString::from(&entry.name));
        }
        if !names.contains(std::ffi::OsStr::new("identity.json")) {
            return Err(Error::new("manifest_invalid"));
        }
        names.extend([MANIFEST.into(), INTENT.into(), CONFIGURATION.into()]);
        if root
            .private_entry_names(&scope, MAX_CAPSULE_FILES)
            .map_err(storage)?
            .into_iter()
            .collect::<BTreeSet<_>>()
            != names
        {
            return Err(Error::new("capsule_contents_changed"));
        }
        let intent = root
            .read_private_metadata(scope.clone(), Path::new(INTENT), MAX_MANIFEST_BYTES)
            .map_err(storage)?;
        let planned: Intent =
            serde_json::from_slice(intent.bytes()).map_err(|_| Error::new("intent_invalid"))?;
        if planned.schema_version != 1
            || planned.configuration_sha256 != decoded.configuration.sha256
            || planned.settings
                != decoded
                    .settings
                    .iter()
                    .map(|entry| (entry.name.clone(), entry.bytes))
                    .collect::<Vec<_>>()
        {
            return Err(Error::new("intent_invalid"));
        }
        let configuration = root
            .read_private_metadata(
                scope.clone(),
                Path::new(CONFIGURATION),
                decoded.configuration.bytes,
            )
            .map_err(storage)?;
        if configuration.bytes().len() as u64 != decoded.configuration.bytes
            || hash(configuration.bytes()) != decoded.configuration.sha256
        {
            return Err(Error::new("configuration_changed"));
        }
        let config: Config = serde_json::from_slice(configuration.bytes())
            .map_err(|_| Error::new("configuration_invalid"))?;
        config
            .validate()
            .map_err(|_| Error::new("configuration_invalid"))?;
        if config.native_settings.is_none() {
            return Err(Error::new("native_settings_owner_required"));
        }
        let mut observed = Vec::with_capacity(decoded.settings.len());
        for entry in &decoded.settings {
            check(deadline, cancelled)?;
            let mut file = root
                .observe_private_metadata(scope.clone(), Path::new(&entry.name), entry.bytes)
                .map_err(storage)?;
            if file.bytes() != entry.bytes {
                return Err(Error::new("settings_changed"));
            }
            file.verify_digest(&entry.sha256)
                .map_err(|_| Error::new("settings_checksum"))?;
            observed.push(file);
        }
        let capsule = Self {
            root,
            scope,
            manifest,
            intent,
            configuration,
            config,
            entries: decoded.settings,
            observed,
        };
        capsule.verify(deadline, cancelled)?;
        Ok(capsule)
    }
    fn verify(&self, deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
        check(deadline, cancelled)?;
        self.manifest.verify().map_err(storage)?;
        self.intent.verify().map_err(storage)?;
        self.configuration.verify().map_err(storage)?;
        for file in &self.observed {
            check(deadline, cancelled)?;
            file.verify().map_err(storage)?;
        }
        let names = self
            .entries
            .iter()
            .map(|entry| std::ffi::OsString::from(&entry.name))
            .chain([MANIFEST.into(), INTENT.into(), CONFIGURATION.into()])
            .collect::<BTreeSet<_>>();
        if self
            .root
            .private_entry_names(&self.scope, MAX_CAPSULE_FILES)
            .map_err(storage)?
            .into_iter()
            .collect::<BTreeSet<_>>()
            != names
        {
            return Err(Error::new("capsule_contents_changed"));
        }
        Ok(())
    }
}

const RESTORE_INTENT: &str = ".juiced-settings-restore.json";
const RESTORE_STAGE: &str = ".juiced-settings-staged.json";
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RestoreIntent {
    schema_version: u32,
    manifest_sha256: String,
    configuration_sha256: String,
    configuration_filename: String,
}

pub fn restore(
    capsule: PathBuf,
    configuration: PathBuf,
    cancelled: Arc<AtomicBool>,
) -> Result<Receipt> {
    restore_before(
        capsule,
        configuration,
        cancelled,
        Instant::now() + Duration::from_secs(30),
    )
}
fn restore_before(
    capsule: PathBuf,
    configuration: PathBuf,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Receipt> {
    restore_checked(capsule, configuration, cancelled, deadline, |_| {})
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RestorePoint {
    AfterIntent,
    AfterFile,
    BeforeConfiguration,
    BeforePublication,
}

fn restore_checked(
    capsule: PathBuf,
    configuration: PathBuf,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    mut checkpoint: impl FnMut(RestorePoint),
) -> Result<Receipt> {
    check(deadline, &cancelled)?;
    let capsule_path = std::path::absolute(capsule).map_err(storage)?;
    let configuration_path = std::path::absolute(configuration).map_err(storage)?;
    let target_path = configuration_path
        .parent()
        .ok_or_else(|| Error::new("destination_invalid"))?;
    let target_grandparent = std::fs::canonicalize(
        target_path
            .parent()
            .ok_or_else(|| Error::new("destination_invalid"))?,
    )
    .map_err(storage)?;
    let target_name = target_path
        .file_name()
        .ok_or_else(|| Error::new("destination_invalid"))?;
    let target_path = target_grandparent.join(target_name);
    let filename = configuration_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            ![
                RESTORE_INTENT,
                RESTORE_STAGE,
                STORE_ROOT,
                crate::storage::CONTROL_FILE,
            ]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
        })
        .ok_or_else(|| Error::new("destination_invalid"))?;
    let capsule_parent = std::fs::canonicalize(
        capsule_path
            .parent()
            .ok_or_else(|| Error::new("capsule_unavailable"))?,
    )
    .map_err(storage)?;
    let capsule_name = capsule_path
        .file_name()
        .ok_or_else(|| Error::new("capsule_unavailable"))?;
    let capsule_path = capsule_parent.join(capsule_name);
    if target_path.starts_with(&capsule_path) || capsule_path.starts_with(&target_path) {
        return Err(Error::new("destination_overlaps_capsule"));
    }
    let parents =
        DirectoryLease::acquire_many([capsule_parent.clone(), target_grandparent.clone()])
            .map_err(storage)?;
    let source_parent = parents
        .iter()
        .find(|root| root.root() == capsule_parent)
        .ok_or_else(|| Error::new("capsule_unavailable"))?
        .clone();
    let target_parent = parents
        .iter()
        .find(|root| root.root() == target_grandparent)
        .ok_or_else(|| Error::new("destination_invalid"))?
        .clone();
    let initial = LiveRoots::new(parents).map_err(storage)?;
    let source = private_child(&source_parent, &initial, Path::new(capsule_name), false)?;
    let reading = LiveRoots::new(vec![
        source_parent.clone(),
        target_parent.clone(),
        source.clone(),
    ])
    .map_err(storage)?;
    let capsule = Capsule::open(source.clone(), reading, deadline, &cancelled)?;
    let output = private_child(&target_parent, &initial, Path::new(target_name), true)?;
    let scope = LiveRoots::new(vec![
        source_parent.clone(),
        target_parent.clone(),
        source.clone(),
        output.clone(),
    ])
    .map_err(storage)?;
    let names = output
        .private_entry_names(&scope, MAX_CAPSULE_FILES)
        .map_err(storage)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let allowed = [RESTORE_INTENT, RESTORE_STAGE, STORE_ROOT, filename]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<BTreeSet<_>>();
    if !names.is_subset(&allowed)
        || (!names.is_empty() && !names.contains(std::ffi::OsStr::new(RESTORE_INTENT)))
    {
        return Err(Error::new("destination_occupied"));
    }
    if names.contains(std::ffi::OsStr::new(filename))
        && names.contains(std::ffi::OsStr::new(RESTORE_STAGE))
    {
        return Err(Error::new("destination_conflict"));
    }
    let manifest_sha256 = hash(capsule.manifest.bytes());
    let configuration_sha256 = hash(capsule.configuration.bytes());
    let planned = RestoreIntent {
        schema_version: 1,
        manifest_sha256: manifest_sha256.clone(),
        configuration_sha256: configuration_sha256.clone(),
        configuration_filename: filename.to_owned(),
    };
    let intent = if names.contains(std::ffi::OsStr::new(RESTORE_INTENT)) {
        let file = output
            .read_private_metadata(scope.clone(), Path::new(RESTORE_INTENT), MAX_MANIFEST_BYTES)
            .map_err(storage)?;
        let existing: RestoreIntent = serde_json::from_slice(file.bytes())
            .map_err(|_| Error::new("restore_intent_invalid"))?;
        if existing.schema_version != 1
            || existing.manifest_sha256 != planned.manifest_sha256
            || existing.configuration_sha256 != planned.configuration_sha256
            || existing.configuration_filename != planned.configuration_filename
        {
            return Err(Error::new("restore_intent_conflict"));
        }
        file
    } else {
        let total = capsule
            .entries
            .iter()
            .try_fold(
                capsule.configuration.bytes().len() as u64,
                |total, entry| total.checked_add(entry.bytes),
            )
            .ok_or_else(|| Error::new("manifest_limit"))?;
        let required = total
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .ok_or_else(|| Error::new("manifest_limit"))?;
        if output.available_private_bytes(&scope).map_err(storage)? < required {
            return Err(Error::new("space_unavailable"));
        }
        output
            .write_new_private_metadata(scope.clone(), Path::new(RESTORE_INTENT), encode(&planned)?)
            .map_err(storage)?
    };
    intent
        .verify_digest(&hash(intent.bytes()))
        .map_err(storage)?;
    intent.sync().map_err(storage)?;
    checkpoint(RestorePoint::AfterIntent);
    let mut existing_configuration = None;
    if names.contains(std::ffi::OsStr::new(filename)) {
        let mut file = output
            .observe_private_metadata(scope.clone(), Path::new(filename), MAXIMUM_CONFIG_BYTES)
            .map_err(storage)?;
        if file.bytes() != capsule.configuration.bytes().len() as u64 {
            return Err(Error::new("configuration_conflict"));
        }
        file.verify_digest(&configuration_sha256)
            .map_err(|_| Error::new("configuration_conflict"))?;
        existing_configuration = Some(file);
    }
    capsule.verify(deadline, &cancelled)?;
    intent.verify().map_err(storage)?;
    let native = private_child(&output, &scope, Path::new(STORE_ROOT), true)?;
    let scope = LiveRoots::new(vec![
        source_parent,
        target_parent,
        source,
        output.clone(),
        native.clone(),
    ])
    .map_err(storage)?;
    let native_names = native
        .private_metadata_entries(&scope, MAX_STORE_FILES)
        .map_err(storage)?
        .into_iter()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    let expected_names = capsule
        .entries
        .iter()
        .map(|entry| std::ffi::OsString::from(&entry.name))
        .collect::<BTreeSet<_>>();
    if !native_names.is_subset(&expected_names) {
        return Err(Error::new("destination_changed"));
    }
    let mut copies = Vec::new();
    let mut retained = Vec::new();
    for entry in &capsule.entries {
        check(deadline, &cancelled)?;
        intent.verify().map_err(storage)?;
        capsule.manifest.verify().map_err(storage)?;
        if native_names.contains(std::ffi::OsStr::new(&entry.name)) {
            let mut file = native
                .observe_private_metadata(scope.clone(), Path::new(&entry.name), entry.bytes)
                .map_err(storage)?;
            if file.bytes() != entry.bytes {
                return Err(Error::new("destination_conflict"));
            }
            file.verify_digest(&entry.sha256)
                .map_err(|_| Error::new("destination_conflict"))?;
            file.sync().map_err(storage)?;
            retained.push(file);
        } else {
            let copy = capsule
                .root
                .copy_new_private_metadata_to(
                    scope.clone(),
                    Path::new(&entry.name),
                    &native,
                    Path::new(&entry.name),
                    entry.bytes,
                )
                .map_err(storage)?;
            if copy.bytes() != entry.bytes || copy.sha256() != entry.sha256 {
                return Err(Error::new("settings_changed"));
            }
            copies.push(copy);
        }
        checkpoint(RestorePoint::AfterFile);
    }
    let binding = capsule
        .config
        .native_settings
        .as_ref()
        .ok_or_else(|| Error::new("native_settings_owner_required"))?;
    let (store, active) = Store::open_bound(native.clone(), scope.clone(), Some(binding), false)
        .map_err(|_| Error::new("settings_invalid"))?;
    let active = active.ok_or_else(|| Error::new("settings_invalid"))?;
    let verify = || -> Result<()> {
        capsule.verify(deadline, &cancelled)?;
        intent.verify().map_err(storage)?;
        for copy in &copies {
            copy.verify().map_err(storage)?;
        }
        for file in &retained {
            file.verify().map_err(storage)?;
        }
        active
            .verify()
            .map_err(|_| Error::new("settings_changed"))?;
        store
            .verify_retained()
            .map_err(|_| Error::new("settings_changed"))?;
        let names = native
            .private_metadata_entries(&scope, MAX_STORE_FILES)
            .map_err(storage)?
            .into_iter()
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>();
        if names != expected_names {
            return Err(Error::new("destination_changed"));
        }
        Ok(())
    };
    verify()?;
    checkpoint(RestorePoint::BeforeConfiguration);
    verify()?;
    let mut published = if let Some(file) = existing_configuration {
        if names.contains(std::ffi::OsStr::new(RESTORE_STAGE)) {
            return Err(Error::new("destination_conflict"));
        }
        file
    } else {
        if !names.contains(std::ffi::OsStr::new(RESTORE_STAGE)) {
            let staged = output
                .stage_new_private_metadata(
                    scope.clone(),
                    Path::new(RESTORE_STAGE),
                    Path::new(filename),
                    capsule.configuration.shared_bytes(),
                )
                .map_err(storage)?;
            staged
                .verify_digest(&configuration_sha256)
                .map_err(storage)?;
        }
        let mut staged = output
            .observe_private_metadata(
                scope.clone(),
                Path::new(RESTORE_STAGE),
                MAXIMUM_CONFIG_BYTES,
            )
            .map_err(storage)?;
        if staged.bytes() != capsule.configuration.bytes().len() as u64 {
            return Err(Error::new("configuration_conflict"));
        }
        staged
            .verify_digest(&configuration_sha256)
            .map_err(|_| Error::new("configuration_conflict"))?;
        checkpoint(RestorePoint::BeforePublication);
        verify()?;
        staged.publish_new(Path::new(filename)).map_err(storage)?
    };
    published
        .verify_digest(&configuration_sha256)
        .map_err(storage)?;
    published.sync().map_err(storage)?;
    verify()?;
    if output
        .private_entry_names(&scope, 3)
        .map_err(storage)?
        .into_iter()
        .collect::<BTreeSet<_>>()
        != [RESTORE_INTENT, STORE_ROOT, filename]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<BTreeSet<_>>()
    {
        return Err(Error::new("destination_changed"));
    }
    Ok(Receipt {
        schema_version: 1,
        scope: "Exact configuration and retained imported-settings store restored; external model artifacts, runtime packages and computation cache require separate recovery",
        settings_files: capsule.entries.len(),
        bytes: capsule.entries.iter().map(|entry| entry.bytes).sum::<u64>()
            + capsule.configuration.bytes().len() as u64,
        manifest_sha256,
        complete: true,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        native_settings::{Document, RecognizedRows, Role, SavedOptions, Settings},
        native_store::{SourceCohort, descriptor::Digest as SettingsDigest},
        storage::MetadataFile,
    };
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

    struct Fixture {
        root: tempfile::TempDir,
        configuration: PathBuf,
        native: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let configuration = root.path().join("source/config.json");
            let lease = DirectoryLease::acquire(configuration.parent().unwrap()).unwrap();
            let initial = LiveRoots::single(lease.clone()).unwrap();
            let native = lease
                .acquire_private_child(&initial, Path::new(STORE_ROOT), true)
                .unwrap();
            let scope = LiveRoots::new(vec![lease.clone(), native.clone()]).unwrap();
            let source = tempfile::tempdir().unwrap();
            fs::write(
                source.path().join("config.json"),
                b"{ \"opaque\": 1e+004 }\n",
            )
            .unwrap();
            fs::write(
                source.path().join("recipe_options.json"),
                b"{\"user.llama\":{\"ctx_size\":512},\"private\":{\"opaque\":1e+004}}\n",
            )
            .unwrap();
            fs::write(
                source.path().join("user_models.json"),
                b"{\"llama\":{\"recipe\":\"llamacpp\"}}",
            )
            .unwrap();
            let source =
                SourceCohort::inspect(source.path(), true, Arc::new(AtomicBool::new(false)))
                    .unwrap();
            let store = Store::open(native.clone(), scope, None, true).unwrap();
            let base = || SettingsDigest::new("a".repeat(64)).unwrap();
            let imported = store
                .prepare_import(store.plan_import(&source, base()).unwrap())
                .unwrap();
            let edit = |active: &crate::native_store::generations::PreparedBinding, threads| {
                let doc = |role| {
                    Document::parse(role, active.file(role).map(MetadataFile::shared_bytes))
                        .unwrap()
                };
                let settings = Settings::from_documents(
                    doc(Role::RecipeOptions),
                    doc(Role::ExecutionProfiles),
                    doc(Role::UserModels),
                    RecognizedRows::new(["user.llama".to_owned()], []).unwrap(),
                )
                .unwrap();
                settings
                    .merge_options(
                        "user.llama",
                        &SavedOptions::parse(format!("{{\"threads\":{threads}}}").as_bytes())
                            .unwrap(),
                    )
                    .unwrap()
                    .document(Role::RecipeOptions)
                    .unwrap()
                    .clone()
            };
            let current = store
                .prepare_edit(
                    store
                        .plan_edit(&imported, &[edit(&imported, 2)], base())
                        .unwrap(),
                )
                .unwrap();
            let _pending = store
                .prepare_edit(
                    store
                        .plan_edit(&current, &[edit(&current, 3)], base())
                        .unwrap(),
                )
                .unwrap();
            let config = serde_json::json!({"schema_version":3,"generation":9,"cache_enabled":false,"inhibit_suspend":false,"resident_budget_bytes":1,"startup_timeout_ms":5000,"request_timeout_ms":5000,"native_settings":current.binding(),"models":[]});
            fs::write(
                &configuration,
                format!(" \n{}\n ", serde_json::to_string(&config).unwrap()),
            )
            .unwrap();
            Self {
                root,
                configuration,
                native: native.root().to_owned(),
            }
        }
        fn image(&self) -> BTreeMap<String, Vec<u8>> {
            let mut image = BTreeMap::from([(
                "configuration".to_owned(),
                fs::read(&self.configuration).unwrap(),
            )]);
            for entry in fs::read_dir(&self.native).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                image.insert(
                    entry.file_name().into_string().unwrap(),
                    fs::read(entry.path()).unwrap(),
                );
            }
            image
        }
        fn destination(&self) -> PathBuf {
            self.root.path().join("backup")
        }
    }

    #[test]
    fn export_preserves_exact_config_original_current_pending_intents_absences_and_private_modes() {
        let fixture = Fixture::new();
        let before = fixture.image();
        let receipt = export(
            fixture.configuration.clone(),
            fixture.destination(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(receipt.complete);
        assert_eq!(fixture.image(), before);
        let output = fixture.destination();
        let raw = fs::read(output.join(MANIFEST)).unwrap();
        let manifest: Manifest = serde_json::from_slice(&raw).unwrap();
        assert_eq!(receipt.manifest_sha256, hash(&raw));
        assert_eq!(receipt.settings_files, before.len() - 1);
        assert_eq!(
            fs::read(output.join(CONFIGURATION)).unwrap(),
            before["configuration"]
        );
        assert_eq!(manifest.settings.len(), before.len() - 1);
        for entry in manifest.settings {
            let bytes = fs::read(output.join(&entry.name)).unwrap();
            assert_eq!(bytes, before[&entry.name]);
            assert_eq!(bytes.len() as u64, entry.bytes);
            assert_eq!(hash(&bytes), entry.sha256);
            assert_eq!(
                fs::metadata(output.join(entry.name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert!(
            before
                .values()
                .any(|bytes| bytes.windows(11).any(|bytes| bytes == b"\"threads\":3"))
        );
    }

    #[test]
    fn export_refuses_live_ownership_cancel_deadline_overlap_and_occupied_destination() {
        let fixture = Fixture::new();
        let before = fixture.image();
        let source = DirectoryLease::acquire(fixture.configuration.parent().unwrap()).unwrap();
        assert!(
            export(
                fixture.configuration.clone(),
                fixture.destination(),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert!(!fixture.destination().exists());
        drop(source);
        assert!(
            export(
                fixture.configuration.clone(),
                fixture.destination(),
                Arc::new(AtomicBool::new(true))
            )
            .is_err()
        );
        assert!(
            export_before(
                fixture.configuration.clone(),
                fixture.destination(),
                Arc::new(AtomicBool::new(false)),
                Instant::now()
            )
            .is_err()
        );
        assert!(
            export(
                fixture.configuration.clone(),
                fixture.native.join("forbidden"),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert!(!fixture.native.join("forbidden").exists());
        assert_eq!(fixture.image(), before);
        fs::create_dir(fixture.destination()).unwrap();
        fs::set_permissions(fixture.destination(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(fixture.destination().join("sentinel"), b"preserved").unwrap();
        assert!(
            export(
                fixture.configuration.clone(),
                fixture.destination(),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert!(!fixture.destination().join(MANIFEST).exists());
        assert_eq!(
            fs::read(fixture.destination().join("sentinel")).unwrap(),
            b"preserved"
        );
        assert_eq!(fixture.image(), before);
    }
    fn restored_image(configuration: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut image =
            BTreeMap::from([("configuration".to_owned(), fs::read(configuration).unwrap())]);
        for entry in fs::read_dir(configuration.parent().unwrap().join(STORE_ROOT)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            image.insert(
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            );
        }
        image
    }
    fn restore_path(fixture: &Fixture) -> PathBuf {
        fixture.root.path().join("restored/config.json")
    }
    fn after_owner_release<T>(
        fixture: &Fixture,
        mut action: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        crate::storage::after_test_release(fixture.root.path(), || match action() {
            Err(error) if error.reason == "owner_busy" => {
                Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
            }
            result => Ok(result),
        })
    }
    fn export_fixture(fixture: &Fixture) {
        after_owner_release(fixture, || {
            export(
                fixture.configuration.clone(),
                fixture.destination(),
                Arc::new(AtomicBool::new(false)),
            )
        })
        .unwrap();
    }
    fn restore_released(
        fixture: &Fixture,
        capsule: PathBuf,
        target: PathBuf,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Receipt> {
        after_owner_release(fixture, || {
            restore(capsule.clone(), target.clone(), cancelled.clone())
        })
    }
    fn restore_checked_released(
        fixture: &Fixture,
        capsule: PathBuf,
        target: PathBuf,
        cancelled: Arc<AtomicBool>,
        deadline: Instant,
        mut checkpoint: impl FnMut(RestorePoint),
    ) -> Result<Receipt> {
        after_owner_release(fixture, || {
            restore_checked(
                capsule.clone(),
                target.clone(),
                cancelled.clone(),
                deadline,
                &mut checkpoint,
            )
        })
    }
    fn restore_fixture(fixture: &Fixture, target: &Path) -> Receipt {
        restore_released(
            fixture,
            fixture.destination(),
            target.to_owned(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
    }

    #[test]
    fn restore_publishes_exact_configuration_last_and_matching_retry_preserves_all_bytes() {
        let fixture = Fixture::new();
        let before = fixture.image();
        export_fixture(&fixture);
        let target = restore_path(&fixture);
        let receipt = restore_fixture(&fixture, &target);
        assert!(receipt.complete);
        assert_eq!(restored_image(&target), before);
        let first = fs::metadata(&target).unwrap();
        let retry = restore_fixture(&fixture, &target);
        assert_eq!(retry.manifest_sha256, receipt.manifest_sha256);
        assert_eq!(
            fs::metadata(&target).unwrap().modified().unwrap(),
            first.modified().unwrap()
        );
        assert_eq!(restored_image(&target), before);
        assert_eq!(fixture.image(), before);
    }

    #[test]
    fn interrupted_restore_retains_matching_files_and_retries_without_early_configuration() {
        for point in [
            RestorePoint::AfterIntent,
            RestorePoint::AfterFile,
            RestorePoint::BeforeConfiguration,
            RestorePoint::BeforePublication,
        ] {
            let fixture = Fixture::new();
            export_fixture(&fixture);
            let before = fixture.image();
            let target = restore_path(&fixture);
            let cancelled = Arc::new(AtomicBool::new(false));
            let token = cancelled.clone();
            let mut reached = false;
            let error = restore_checked_released(
                &fixture,
                fixture.destination(),
                target.clone(),
                cancelled,
                Instant::now() + Duration::from_secs(30),
                |current| {
                    if current == point && !reached {
                        reached = true;
                        assert!(!target.exists());
                        token.store(true, Ordering::Release);
                    }
                },
            )
            .unwrap_err();
            assert!(reached, "restore never reached its fault point: {error:?}");
            assert_eq!(error.reason, "cancelled");
            assert!(!target.exists());
            assert!(target.parent().unwrap().join(RESTORE_INTENT).is_file());
            restore_fixture(&fixture, &target);
            assert_eq!(restored_image(&target), before);
            assert_eq!(fixture.image(), before);
        }
    }

    #[test]
    fn retry_refuses_conflicting_retained_file_without_replacing_it_or_publishing_configuration() {
        let fixture = Fixture::new();
        export_fixture(&fixture);
        let before = fixture.image();
        let target = restore_path(&fixture);
        let token = Arc::new(AtomicBool::new(false));
        let cancelled = token.clone();
        assert!(
            restore_checked_released(
                &fixture,
                fixture.destination(),
                target.clone(),
                token,
                Instant::now() + Duration::from_secs(30),
                |point| {
                    if point == RestorePoint::AfterFile {
                        cancelled.store(true, Ordering::Release);
                    }
                }
            )
            .is_err()
        );
        let native = target.parent().unwrap().join(STORE_ROOT);
        let path = fs::read_dir(native)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| !path.file_name().unwrap().to_string_lossy().starts_with('.'))
            .unwrap();
        let changed = vec![b'!'; fs::metadata(&path).unwrap().len() as usize];
        fs::write(&path, &changed).unwrap();
        assert!(
            restore_released(
                &fixture,
                fixture.destination(),
                target.clone(),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), changed);
        assert!(!target.exists());
        assert_eq!(fixture.image(), before);
    }

    #[test]
    fn corrupt_missing_unknown_and_duplicate_capsule_inputs_reject_before_target_creation() {
        for variant in ["checksum", "missing", "unknown", "duplicate"] {
            let fixture = Fixture::new();
            export_fixture(&fixture);
            let target = restore_path(&fixture);
            let before = fixture.image();
            let raw = fs::read(fixture.destination().join(MANIFEST)).unwrap();
            let mut manifest: Manifest = serde_json::from_slice(&raw).unwrap();
            match variant {
                "checksum" => {
                    let path = fixture.destination().join(&manifest.settings[0].name);
                    let bytes = vec![b'!'; manifest.settings[0].bytes as usize];
                    fs::write(path, bytes).unwrap();
                }
                "missing" => {
                    fs::remove_file(fixture.destination().join(&manifest.settings[0].name))
                        .unwrap();
                }
                "unknown" => {
                    fs::write(fixture.destination().join("foreign"), b"preserved").unwrap();
                }
                "duplicate" => {
                    manifest.settings.push(manifest.settings[0].clone());
                    fs::write(
                        fixture.destination().join(MANIFEST),
                        serde_json::to_vec(&manifest).unwrap(),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let error = restore_released(
                &fixture,
                fixture.destination(),
                target.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
            assert!(!target.parent().unwrap().exists());
            assert_eq!(fixture.image(), before);
            assert!(!error.to_string().contains("opaque"));
        }
    }
    fn settings_snapshot(configuration: &Path) -> crate::native_store::generations::Snapshot {
        let binding = crate::storage::after_test_release(configuration.parent().unwrap(), || {
            ConfigurationBinding::claim(configuration.to_owned())
        });
        let config = binding.read().unwrap();
        let initial = LiveRoots::single(binding.root.clone()).unwrap();
        let native = binding
            .root
            .acquire_private_child(&initial, Path::new(STORE_ROOT), false)
            .unwrap();
        let scope = LiveRoots::new(vec![binding.root, native.clone()]).unwrap();
        let store = Store::open(native, scope, config.native_settings.as_ref(), false).unwrap();
        store.snapshot().unwrap()
    }
    #[test]
    fn pending_missing_files_and_unreferenced_quarantine_bytes_keep_their_ownership_charges() {
        let fixture = Fixture::new();
        let pending = fixture
            .image()
            .into_iter()
            .find(|(_, bytes)| bytes.windows(11).any(|bytes| bytes == b"\"threads\":3"))
            .unwrap()
            .0;
        fs::remove_file(fixture.native.join(&pending)).unwrap();
        let orphan = "blob-ffffffffffffffffffffffffffffffff";
        fs::write(fixture.native.join(orphan), b"partial owned bytes").unwrap();
        fs::set_permissions(
            fixture.native.join(orphan),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let before = fixture.image();
        let accounting = settings_snapshot(&fixture.configuration);
        export_fixture(&fixture);
        let target = restore_path(&fixture);
        restore_released(
            &fixture,
            fixture.destination(),
            target.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(restored_image(&target), before);
        assert_eq!(settings_snapshot(&target), accounting);
        assert!(
            !target
                .parent()
                .unwrap()
                .join(STORE_ROOT)
                .join(pending)
                .exists()
        );
        assert_eq!(
            fs::read(target.parent().unwrap().join(STORE_ROOT).join(orphan)).unwrap(),
            b"partial owned bytes"
        );
    }
    #[test]
    fn configuration_conflict_at_publication_preserves_both_foreign_primary_and_verified_stage() {
        let fixture = Fixture::new();
        export_fixture(&fixture);
        let target = restore_path(&fixture);
        let source = fixture.image();
        let error = restore_checked_released(
            &fixture,
            fixture.destination(),
            target.clone(),
            Arc::new(AtomicBool::new(false)),
            Instant::now() + Duration::from_secs(30),
            |point| {
                if point == RestorePoint::BeforePublication {
                    fs::write(&target, b"foreign primary").unwrap();
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.reason, "destination_conflict");
        assert_eq!(fs::read(&target).unwrap(), b"foreign primary");
        assert_eq!(
            fs::read(target.parent().unwrap().join(RESTORE_STAGE)).unwrap(),
            source["configuration"]
        );
        assert_eq!(fixture.image(), source);
    }
    #[test]
    fn reserved_primary_names_and_ascii_case_variants_refuse_before_target_creation() {
        let fixture = Fixture::new();
        export_fixture(&fixture);
        for name in [
            RESTORE_STAGE.to_owned(),
            RESTORE_STAGE.to_ascii_uppercase(),
            RESTORE_INTENT.to_ascii_uppercase(),
            STORE_ROOT.to_ascii_uppercase(),
            crate::storage::CONTROL_FILE.to_ascii_uppercase(),
        ] {
            let target = fixture.root.path().join("refused").join(name);
            let error = restore_released(
                &fixture,
                fixture.destination(),
                target.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
            assert_eq!(error.reason, "destination_invalid");
            assert!(!target.parent().unwrap().exists());
        }
    }
}
