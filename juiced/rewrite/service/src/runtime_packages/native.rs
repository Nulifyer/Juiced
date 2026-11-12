use super::*;
pub(super) mod managed;
use crate::runtime_bundle::FileRole;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io,
    path::{Component, Path},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const STORE: &str = "runtime-packages";
const MANIFEST: &str = "packages.json";
const MAX_INPUT: u64 = 2 * 1024 * 1024;
const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const MAX_PACKAGES: usize = 32;
const MAX_STORE_BYTES: u64 = 32 * 1024 * 1024 * 1024;

fn storage(error: io::Error) -> Error {
    if crate::storage::is_root_unavailable(&error) {
        return Error::new("storage_root_unavailable");
    }
    Error::new(match error.kind() {
        io::ErrorKind::WouldBlock => "owner_busy",
        io::ErrorKind::Unsupported => "platform_unsupported",
        io::ErrorKind::Interrupted => "cancelled",
        io::ErrorKind::TimedOut => "deadline",
        io::ErrorKind::AlreadyExists => "destination_conflict",
        _ => "storage_unavailable",
    })
}
fn token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:+-@".contains(&byte))
}
fn member_path(path: &Path) -> bool {
    path.as_os_str().len()<=4096 && path.components().count()<=32 && path.components().all(|part| matches!(part,Component::Normal(name) if !name.to_string_lossy().starts_with(".juiced-")))
}
struct HashWriter<'a>(&'a mut Sha256);
impl io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl ImportRequest {
    fn canonicalize(&mut self) {
        self.bundle.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.provenance.declared_licenses.sort();
        self.declared_target.compute_paths.sort();
    }
    fn validate(&self) -> Result<u64> {
        let invalid = || Error::new("invalid_import");
        let provider = match self.bundle.implementation_id.as_str() {
            "llama-cpp" => crate::config::Provider::LlamaCpp,
            "whisper-cpp" => crate::config::Provider::WhisperCpp,
            _ => return Err(invalid()),
        };
        self.bundle.validate(provider).map_err(|_| invalid())?;
        if self.schema_version != 1
            || !token(&self.provenance.builder_id, 128)
            || !token(&self.provenance.source_revision, 256)
            || !token(&self.provenance.build_revision, 256)
            || self.provenance.declared_licenses.len() > 16
            || self.provenance.declared_licenses.iter().any(|license| {
                license.is_empty()
                    || license.len() > 128
                    || !license
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            })
            || self
                .provenance
                .declared_licenses
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || !token(&self.declared_target.operating_system, 64)
            || !token(&self.declared_target.architecture, 64)
            || !token(&self.declared_target.abi, 64)
            || self.declared_target.compute_paths.is_empty()
            || self.declared_target.compute_paths.len() > 16
            || self
                .declared_target
                .compute_paths
                .iter()
                .any(|path| !token(path, 64))
            || self
                .declared_target
                .compute_paths
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self.files.len() != self.bundle.files.len()
            || self
                .bundle
                .library_directories
                .iter()
                .any(|path| path != Path::new(".") && !member_path(path))
            || self
                .bundle
                .files
                .windows(2)
                .any(|pair| pair[0].path >= pair[1].path)
            || self
                .files
                .windows(2)
                .any(|pair| pair[0].path >= pair[1].path)
            || self.provenance.declared_sbom.as_ref().is_some_and(|path| {
                !self
                    .bundle
                    .files
                    .iter()
                    .any(|file| file.path == *path && file.role == FileRole::Resource)
            })
            || self
                .provenance
                .release
                .as_ref()
                .is_some_and(|release| !release.valid())
        {
            return Err(invalid());
        }
        let mut total = 0u64;
        for (file, size) in self.bundle.files.iter().zip(&self.files) {
            if file.path != size.path
                || !member_path(&file.path)
                || size.bytes > crate::runtime_bundle::MAX_FILE_BYTES
                || (size.bytes == 0 && file.role != FileRole::Resource)
            {
                return Err(invalid());
            }
            total = total
                .checked_add(size.bytes)
                .filter(|bytes| *bytes <= crate::runtime_bundle::MAX_TOTAL_BYTES)
                .ok_or_else(invalid)?;
        }
        if total == 0 {
            return Err(invalid());
        }
        Ok(total)
    }
    fn identity(&self) -> Result<String> {
        let mut hash = Sha256::new();
        hash.update(b"juiced-runtime-package-v1\0");
        serde_json::to_writer(
            HashWriter(&mut hash),
            &(
                self.schema_version,
                self.bundle.schema_version,
                &self.bundle.implementation_id,
                &self.bundle.entrypoint,
                &self.bundle.library_directories,
                &self.bundle.files,
                &self.provenance,
                &self.declared_target,
                &self.files,
            ),
        )
        .map_err(|_| Error::new("invalid_import"))?;
        Ok(format!("package-{:x}", hash.finalize()))
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Staging,
    Installed,
}
#[derive(Deserialize, Serialize)]
#[serde(transparent)]
struct Record {
    request: ImportRequest,
    #[serde(skip)]
    binding_leases: std::sync::atomic::AtomicUsize,
}
impl From<ImportRequest> for Record {
    fn from(request: ImportRequest) -> Self {
        Self {
            request,
            binding_leases: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}
impl std::ops::Deref for Record {
    type Target = ImportRequest;
    fn deref(&self) -> &ImportRequest {
        &self.request
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Package {
    id: String,
    specification: Arc<Record>,
    phase: Phase,
    reserved_bytes: u64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    generation: u64,
    packages: Vec<Package>,
}
impl Manifest {
    fn validate(&self) -> Result<u64> {
        if self.schema_version != 1 || self.packages.len() > MAX_PACKAGES {
            return Err(Error::new("manifest_invalid"));
        }
        let mut ids = BTreeSet::new();
        let mut total = 0u64;
        for package in &self.packages {
            let bytes = package.specification.validate()?;
            if package.specification.bundle.root != Path::new(".")
                || package.id != package.specification.identity()?
                || !ids.insert(package.id.as_str())
                || bytes != package.reserved_bytes
            {
                return Err(Error::new("manifest_invalid"));
            }
            total = total
                .checked_add(bytes)
                .filter(|bytes| *bytes <= MAX_STORE_BYTES)
                .ok_or_else(|| Error::new("store_limit"))?;
        }
        Ok(total)
    }
}

use crate::{
    config::ConfigurationBinding,
    storage::{
        DirectoryLease, DirectoryReader, LiveRoots,
        runtime_payload::{Member, RuntimePayload},
    },
};

struct Operation {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    Publishing(u64),
    Reserved,
    Copying(usize),
    Copied(usize),
    Sealed,
}
impl Operation {
    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        Ok(())
    }
}
fn commit(
    owner: &DirectoryLease,
    scope: &LiveRoots,
    next: &Manifest,
    operation: &Operation,
) -> Result<()> {
    operation.check().map_err(storage)?;
    next.validate()?;
    crate::storage::check_json_size(next, MAX_MANIFEST)
        .map_err(|_| Error::new("manifest_limit"))?;
    scope.write_json(owner, MANIFEST, next).map_err(|error| {
        Error::new(if error.committed() {
            "manifest_durability_uncertain"
        } else if error.root_unavailable() {
            "storage_root_unavailable"
        } else {
            "manifest_persistence"
        })
    })?;
    Ok(())
}
pub(super) fn import_file(
    configuration: PathBuf,
    input: PathBuf,
    cancelled: Arc<AtomicBool>,
) -> Result<ImportReceipt> {
    import_checked(configuration, input, cancelled, |_| {})
}
pub(super) fn import_online(
    slot: &std::sync::OnceLock<Arc<managed::Store>>,
    root: DirectoryLease,
    scope: Arc<LiveRoots>,
    input: PathBuf,
    expected_generation: u64,
    cancelled: Arc<AtomicBool>,
    control: Arc<crate::runtime_packages::memory::Controller>,
) -> Result<ImportReceipt> {
    if !input.is_absolute() {
        return Err(Error::new("invalid_import"));
    }
    if slot.get().is_none() && expected_generation != 0 {
        return Err(Error::new("manifest_revision_conflict"));
    }
    let operation = Operation {
        deadline: Instant::now() + Duration::from_secs(300),
        cancelled,
    };
    let input = prepare_input(input, &operation)?;
    let store = if let Some(store) = slot.get() {
        store.clone()
    } else {
        let store = managed::Store::open_for_import_accounted(root, scope, control)?;
        slot.set(store.clone())
            .map_err(|_| Error::new("runtime_import_busy"))?;
        store
    };
    import_prepared(&store, input, &operation, Some(expected_generation), |_| {})
}
struct PreparedInput {
    observed: crate::storage::ReadObservation,
    request: ImportRequest,
    source_bundle: BundleSpec,
    id: String,
    bytes: u64,
}
fn prepare_input(input: PathBuf, operation: &Operation) -> Result<PreparedInput> {
    operation.check().map_err(storage)?;
    let input = std::path::absolute(input).map_err(storage)?;
    let reader = DirectoryReader::open_existing(
        input
            .parent()
            .ok_or_else(|| Error::new("input_unavailable"))?,
    )
    .map_err(storage)?;
    let observed = reader
        .observe(
            input
                .file_name()
                .ok_or_else(|| Error::new("input_unavailable"))?,
            MAX_INPUT,
        )
        .map_err(storage)?;
    let mut request: ImportRequest = serde_json::from_slice(
        observed
            .bytes()
            .ok_or_else(|| Error::new("input_unavailable"))?,
    )
    .map_err(|_| Error::new("invalid_import"))?;
    request.canonicalize();
    let bytes = request.validate()?;
    if request.bundle.root.is_relative() {
        request.bundle.root = reader.root().join(&request.bundle.root);
    }
    let source_bundle = request.bundle.clone();
    request.bundle.root = PathBuf::from(".");
    let id = request.identity()?;
    Ok(PreparedInput {
        observed,
        request,
        source_bundle,
        id,
        bytes,
    })
}
fn import_checked(
    configuration: PathBuf,
    input: PathBuf,
    cancelled: Arc<AtomicBool>,
    checkpoint: impl FnMut(Point),
) -> Result<ImportReceipt> {
    let operation = Operation {
        deadline: Instant::now() + Duration::from_secs(300),
        cancelled,
    };
    let input = prepare_input(input, &operation)?;
    let configuration = ConfigurationBinding::claim(configuration).map_err(|error| {
        error.downcast_ref::<io::Error>().map_or_else(
            || Error::new("configuration_unavailable"),
            |error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    Error::new("owner_busy")
                } else {
                    Error::new("configuration_unavailable")
                }
            },
        )
    })?;
    configuration
        .read()
        .map_err(|_| Error::new("configuration_invalid"))?;
    let scope = LiveRoots::single(configuration.root.clone()).map_err(storage)?;
    let store = managed::Store::open_for_import(configuration.root.clone(), scope)?;
    import_prepared(&store, input, &operation, None, checkpoint)
}
fn import_prepared(
    store: &managed::Store,
    input: PreparedInput,
    operation: &Operation,
    expected_generation: Option<u64>,
    mut checkpoint: impl FnMut(Point),
) -> Result<ImportReceipt> {
    let _importing = store.enter_import()?;
    operation.check().map_err(storage)?;
    let PreparedInput {
        observed,
        request,
        source_bundle,
        id,
        bytes,
    } = input;
    let owner = store.owner.clone();
    let scope = store.scope.clone();
    let mut manifest = store.snapshot()?;
    if expected_generation.is_some_and(|expected| expected != manifest.generation) {
        return Err(Error::new("manifest_revision_conflict"));
    }
    let retained_bytes = manifest.validate()?;
    validate_index(&owner, &scope, &manifest)?;
    let mut index = manifest
        .packages
        .iter()
        .position(|package| package.id == id);
    let installed = index.is_some_and(|index| manifest.packages[index].phase == Phase::Installed);
    let record = match index {
        Some(index) => manifest.packages[index].specification.clone(),
        None => Arc::new(Record::from(request)),
    };
    let request = &record.request;
    if index.is_none() {
        if manifest.packages.len() >= MAX_PACKAGES
            || retained_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > MAX_STORE_BYTES)
        {
            return Err(Error::new("store_limit"));
        }
        let needed = bytes
            .checked_add(MAX_MANIFEST * 2)
            .ok_or_else(|| Error::new("store_limit"))?;
        if owner.available_private_bytes(&scope).map_err(storage)? < needed {
            return Err(Error::new("space_unavailable"));
        }
    }
    let source = if installed {
        None
    } else {
        source_bundle
            .verify(operation.deadline)
            .map_err(|_| Error::new("source_bundle_unavailable"))?;
        let source = DirectoryReader::open_existing(&source_bundle.root).map_err(storage)?;
        for file in &request.files {
            operation.check().map_err(storage)?;
            let metadata =
                std::fs::symlink_metadata(source.root().join(&file.path)).map_err(storage)?;
            if !metadata.is_file() || metadata.len() != file.bytes {
                return Err(Error::new("source_size_changed"));
            }
        }
        Some(source)
    };
    observed.verify().map_err(storage)?;
    if index.is_none() {
        manifest.generation = manifest
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::new("generation_exhausted"))?;
        manifest.packages.push(Package {
            id: id.clone(),
            specification: record.clone(),
            phase: Phase::Staging,
            reserved_bytes: bytes,
        });
        store.publish(manifest.clone(), operation, || {
            checkpoint(Point::Publishing(manifest.generation))
        })?;
        index = Some(manifest.packages.len() - 1);
    }
    if !installed {
        checkpoint(Point::Reserved);
    }
    operation.check().map_err(storage)?;
    let package_owner = owner
        .acquire_private_child(&scope, Path::new(&id), !installed)
        .map_err(storage)?;
    let payload_scope = scope.extend(package_owner.clone()).map_err(storage)?;
    let payload =
        RuntimePayload::open(package_owner, payload_scope, !installed).map_err(storage)?;
    let mut reused_files = 0;
    if let Some(source) = &source {
        payload
            .prepare_library_directories(&request.bundle.library_directories)
            .map_err(storage)?;
        for (index, (file, size)) in request.bundle.files.iter().zip(&request.files).enumerate() {
            reused_files += usize::from(
                payload
                    .copy_member(
                        source,
                        Member {
                            path: &file.path,
                            bytes: size.bytes,
                            sha256: &file.sha256,
                            executable: file.role == FileRole::Executable,
                        },
                        || {
                            checkpoint(Point::Copying(index));
                            operation.check()
                        },
                    )
                    .map_err(storage)?,
            );
            checkpoint(Point::Copied(index));
        }
    }
    let mut bundle = request.bundle.clone();
    bundle.root = payload.path();
    payload.verify().map_err(storage)?;
    observed.verify().map_err(storage)?;
    operation.check().map_err(storage)?;
    if !installed {
        source_bundle
            .verify(operation.deadline)
            .map_err(|_| Error::new("source_bundle_changed"))?;
        payload
            .seal(
                request
                    .bundle
                    .files
                    .iter()
                    .filter_map(|file| file.path.parent())
                    .chain(
                        request
                            .bundle
                            .library_directories
                            .iter()
                            .map(|path| path.as_path()),
                    ),
            )
            .map_err(storage)?;
        checkpoint(Point::Sealed);
    }
    operation.check().map_err(storage)?;
    let verified = bundle
        .verify(operation.deadline)
        .map_err(|_| Error::new("payload_verification"))?;
    payload.verify().map_err(storage)?;
    observed.verify().map_err(storage)?;
    operation.check().map_err(storage)?;
    if !installed {
        verify_sealed(&payload, request)?;
        manifest.packages[index.expect("staged package")].phase = Phase::Installed;
        manifest.generation = manifest
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::new("generation_exhausted"))?;
        store.publish(manifest.clone(), operation, || {
            checkpoint(Point::Publishing(manifest.generation))
        })?;
    }
    verify_sealed(&payload, request)?;
    Ok(ImportReceipt {
        schema_version: 1,
        package: id,
        installed: true,
        newly_installed: !installed,
        reserved_bytes: bytes,
        reused_files,
        bundle_digest: verified.digest,
        bundle,
        compatibility: "unqualified",
        scope: "Exact local runtime-file import only. Declared provenance/target are not qualification or a writer fence; model/profile selection and online promotion remain explicit separate operations.",
    })
}

fn verify_sealed(payload: &RuntimePayload, request: &ImportRequest) -> Result<()> {
    payload
        .verify_sealed(
            request
                .bundle
                .files
                .iter()
                .zip(&request.files)
                .map(|(file, size)| Member {
                    path: &file.path,
                    bytes: size.bytes,
                    sha256: &file.sha256,
                    executable: file.role == FileRole::Executable,
                }),
            &request.bundle.library_directories,
        )
        .map_err(|error| {
            if crate::storage::is_root_unavailable(&error) {
                storage(error)
            } else {
                Error::new("payload_ownership_changed")
            }
        })
}

fn validate_index(owner: &DirectoryLease, scope: &LiveRoots, manifest: &Manifest) -> Result<()> {
    for name in owner
        .private_entry_names(scope, MAX_PACKAGES + 2)
        .map_err(storage)?
    {
        let name = name.to_str().ok_or_else(|| Error::new("store_unindexed"))?;
        if name != MANIFEST && !manifest.packages.iter().any(|package| package.id == name) {
            return Err(Error::new("store_unindexed"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
