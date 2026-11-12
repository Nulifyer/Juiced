use super::{
    CacheError, HashBudget, MANIFEST, SnapshotState, decode_manifest_checked, hash_retained,
    record_charge, validate_id, validate_manifest,
};
use crate::storage::same_file;
use serde::Serialize;
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct SnapshotInspectionOptions {
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
    pub maximum_manifest_bytes: u64,
    pub maximum_records: usize,
    pub maximum_files: usize,
    pub maximum_issues: usize,
    pub maximum_hash_bytes: u64,
    pub maximum_snapshot_bytes: u64,
}

impl Default for SnapshotInspectionOptions {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: Arc::new(AtomicBool::new(false)),
            maximum_manifest_bytes: 16 * 1024 * 1024,
            maximum_records: 16384,
            maximum_files: 16384,
            maximum_issues: 256,
            maximum_hash_bytes: 64 * 1024 * 1024 * 1024,
            maximum_snapshot_bytes: 64 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotInspectionStatus {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotInspectionIssue {
    pub code: &'static str,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OfflineSnapshotObservation {
    pub id: String,
    pub state: SnapshotState,
    pub reserved_bytes: u64,
    pub bytes: Option<u64>,
    pub checksum_valid: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct SnapshotStoreInspection {
    pub format: &'static str,
    pub format_recognized: bool,
    pub status: SnapshotInspectionStatus,
    pub read_only: bool,
    pub non_atomic: bool,
    pub cooperative_deadline: bool,
    pub no_recovery_authority: bool,
    pub recovery_authorized: bool,
    pub application_authorized: bool,
    pub writer_termination_verified: bool,
    pub validation_complete: bool,
    pub root_verified: bool,
    pub manifest_verified: bool,
    pub inventory_complete: bool,
    pub generation: Option<u64>,
    pub charged_bytes: Option<u64>,
    pub records_checked: usize,
    pub files_enumerated: usize,
    pub files_checked: usize,
    pub manifest_bytes: u64,
    pub hash_bytes: u64,
    pub journal_bytes: u64,
    pub journals_checked: usize,
    pub retained_readers: usize,
    pub issues_truncated: bool,
    pub snapshots: Vec<OfflineSnapshotObservation>,
    pub issues: Vec<SnapshotInspectionIssue>,
}

impl SnapshotStoreInspection {
    fn new() -> Self {
        Self {
            format: "juiced-rust-cache-v1",
            format_recognized: false,
            status: SnapshotInspectionStatus::Unknown,
            read_only: true,
            non_atomic: true,
            cooperative_deadline: true,
            no_recovery_authority: true,
            recovery_authorized: false,
            application_authorized: false,
            writer_termination_verified: false,
            validation_complete: false,
            root_verified: false,
            manifest_verified: false,
            inventory_complete: false,
            generation: None,
            charged_bytes: None,
            records_checked: 0,
            files_enumerated: 0,
            files_checked: 0,
            manifest_bytes: 0,
            hash_bytes: 0,
            journal_bytes: 0,
            journals_checked: 0,
            retained_readers: 0,
            issues_truncated: false,
            snapshots: Vec::new(),
            issues: Vec::new(),
        }
    }

    fn issue(
        &mut self,
        options: &SnapshotInspectionOptions,
        code: &'static str,
        path: Option<&str>,
    ) {
        if self.issues.len() >= options.maximum_issues {
            self.issues_truncated = true;
            return;
        }
        let path = path.map(|path| {
            if path == MANIFEST || snapshot_id(path).is_some() {
                path.to_owned()
            } else {
                "<unrecognized>".to_owned()
            }
        });
        self.issues.push(SnapshotInspectionIssue { code, path });
    }

    fn invalidate(&mut self) {
        self.status = SnapshotInspectionStatus::Unknown;
        self.validation_complete = false;
        self.manifest_verified = false;
        self.charged_bytes = None;
        for snapshot in &mut self.snapshots {
            snapshot.checksum_valid = None;
        }
    }
}

/// Reads only the Rust cache format, without writer ownership or recovery authority.
/// Cancellation and deadlines cannot interrupt blocked kernel I/O. Reads may update atime.
pub fn inspect_snapshot_store(
    root: impl AsRef<Path>,
    options: &SnapshotInspectionOptions,
) -> SnapshotStoreInspection {
    let mut report = SnapshotStoreInspection::new();
    if options.maximum_manifest_bytes == 0
        || options.maximum_manifest_bytes > 16 * 1024 * 1024
        || options.maximum_records == 0
        || options.maximum_records > 16384
        || options.maximum_files == 0
        || options.maximum_files > 65536
        || options.maximum_issues == 0
        || options.maximum_issues > 1024
        || options.maximum_snapshot_bytes == 0
    {
        report.issue(options, "invalid_inspection_limits", None);
        return report;
    }
    if let Err(code) = check(options) {
        report.issue(options, code, None);
        return report;
    }
    let root = match DirectoryInput::open(root.as_ref(), options) {
        Ok(root) => root,
        Err(_) => {
            report.issue(
                options,
                check(options)
                    .err()
                    .unwrap_or("root_unavailable_or_invalid"),
                None,
            );
            return report;
        }
    };
    inspect_directory(root, options, report)
}

fn check(options: &SnapshotInspectionOptions) -> Result<(), &'static str> {
    if options.cancelled.load(Ordering::Relaxed) {
        Err("inspection_cancelled")
    } else if Instant::now() >= options.deadline {
        Err("inspection_deadline")
    } else {
        Ok(())
    }
}

fn inspection_io_check(options: &SnapshotInspectionOptions) -> io::Result<()> {
    check(options).map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "Inspection stopped"))
}

fn inspect_directory(
    root: DirectoryInput,
    options: &SnapshotInspectionOptions,
    mut report: SnapshotStoreInspection,
) -> SnapshotStoreInspection {
    if root.verify().is_err() {
        report.issue(options, "root_changed_or_unverified", None);
        return report;
    }
    report.root_verified = true;
    let mut manifest_file = match root.open_file(MANIFEST) {
        Ok(file) => file,
        Err(_) => {
            report.issue(options, "manifest_unavailable_or_invalid", Some(MANIFEST));
            return report;
        }
    };
    let before = match FileStamp::read(&manifest_file, false) {
        Ok(stamp) => stamp,
        Err(_) => {
            report.issue(options, "manifest_unavailable_or_invalid", Some(MANIFEST));
            return report;
        }
    };
    if before.bytes > options.maximum_manifest_bytes {
        report.issue(options, "manifest_size_limit", Some(MANIFEST));
        return report;
    }
    if root.verify_file(MANIFEST, &manifest_file, &before).is_err() {
        report.issue(options, "manifest_changed_or_unverified", Some(MANIFEST));
        return report;
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(before.bytes as usize).is_err() {
        report.issue(options, "inspection_allocation_limit", None);
        return report;
    }
    let read_result = (|| {
        let mut buffer = [0u8; 65536];
        while report.manifest_bytes < before.bytes {
            check(options)?;
            let request = (before.bytes - report.manifest_bytes).min(buffer.len() as u64) as usize;
            let count = manifest_file
                .read(&mut buffer[..request])
                .map_err(|_| "manifest_unreadable")?;
            if count == 0 {
                return Err("manifest_changed_or_unverified");
            }
            bytes.extend_from_slice(&buffer[..count]);
            report.manifest_bytes += count as u64;
        }
        check(options)?;
        root.verify_file(MANIFEST, &manifest_file, &before)
            .map_err(|_| "manifest_changed_or_unverified")
    })();
    if let Err(code) = read_result {
        report.issue(options, code, Some(MANIFEST));
        return report;
    }
    let mut manifest = match decode_manifest_checked(&bytes, options.maximum_records, &|| {
        check(options).map_err(|_| CacheError::Limit)
    })
    .and_then(|manifest| {
        validate_manifest(
            &manifest,
            options.maximum_records,
            options.maximum_snapshot_bytes,
        )?;
        Ok(manifest)
    }) {
        Ok(manifest) => manifest,
        Err(_) => {
            report.issue(
                options,
                check(options).err().unwrap_or("manifest_invalid"),
                Some(MANIFEST),
            );
            return report;
        }
    };
    report.files_checked = 1;
    report.generation = Some(manifest.generation);
    report.format_recognized = true;
    report.manifest_verified = true;
    report.status = SnapshotInspectionStatus::Partial;
    let journals = match inspect_recovery_journals(&root, &mut manifest, options, &mut report) {
        Ok(journals) => journals,
        Err(code) => {
            report.issue(options, code, None);
            report.charged_bytes = None;
            return report;
        }
    };
    if validate_manifest(
        &manifest,
        options.maximum_records,
        options.maximum_snapshot_bytes,
    )
    .is_err()
    {
        report.issue(options, "recovery_journal_invalid", None);
        return report;
    }
    report.retained_readers = manifest.reader_intents.len();
    if report.retained_readers != 0 {
        report.issue(options, "unresolved_snapshot_reader", None);
    }
    let mut budget = HashBudget {
        remaining: options.maximum_hash_bytes,
        deadline: options.deadline,
        cancelled: Some(options.cancelled.clone()),
    };
    let mut charged = Some(0u64);
    let mut checks_complete = true;
    for (id, record) in &manifest.entries {
        if let Err(code) = check(options) {
            report.issue(options, code, None);
            checks_complete = false;
            break;
        }
        if report.files_checked >= options.maximum_files {
            report.issue(options, "inspection_file_limit", None);
            checks_complete = false;
            break;
        }
        let name = format!("kv-{id}.bin");
        let mut observation = OfflineSnapshotObservation {
            id: id.clone(),
            state: record.state,
            reserved_bytes: record.reserved_bytes,
            bytes: record.bytes,
            checksum_valid: None,
        };
        let mut actual = 0;
        match root.open_file(&name) {
            Ok(mut file) => match FileStamp::read(&file, false) {
                Ok(stamp) => {
                    actual = stamp.bytes;
                    report.files_checked += 1;
                    if root.verify_file(&name, &file, &stamp).is_err() {
                        report.issue(options, "snapshot_changed_or_unverified", Some(&name));
                        charged = None;
                        checks_complete = false;
                    } else if record.state == SnapshotState::Saved {
                        match hash_retained(&mut file, options.maximum_snapshot_bytes, &mut budget)
                        {
                            Ok((size, hash)) if root.verify_file(&name, &file, &stamp).is_ok() => {
                                let valid = record.bytes == Some(size)
                                    && record.sha256.as_ref() == Some(&hash);
                                observation.checksum_valid = Some(valid);
                                if !valid {
                                    report.issue(
                                        options,
                                        "snapshot_checksum_mismatch",
                                        Some(&name),
                                    );
                                }
                            }
                            Err(CacheError::Limit) => {
                                let code = check(options).err().unwrap_or("inspection_hash_limit");
                                report.issue(options, code, Some(&name));
                                checks_complete = false;
                            }
                            _ => {
                                report.issue(
                                    options,
                                    "snapshot_changed_or_unverified",
                                    Some(&name),
                                );
                                charged = None;
                                checks_complete = false;
                            }
                        }
                    } else if root.verify_file(&name, &file, &stamp).is_err() {
                        report.issue(options, "snapshot_changed_or_unverified", Some(&name));
                        checks_complete = false;
                    }
                }
                Err(_) => {
                    report.issue(options, "snapshot_file_unavailable_or_invalid", Some(&name));
                    charged = None;
                    checks_complete = false;
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if record.state == SnapshotState::Saved {
                    report.issue(options, "snapshot_file_missing", Some(&name));
                }
            }
            Err(_) => {
                report.issue(options, "snapshot_file_unavailable_or_invalid", Some(&name));
                charged = None;
                checks_complete = false;
            }
        }
        if record.state != SnapshotState::Saved {
            report.issue(options, "unresolved_snapshot_operation", Some(&name));
            checks_complete = false;
        }
        charged = charged.and_then(|total| total.checked_add(record_charge(record, actual)));
        report.records_checked += 1;
        report.snapshots.push(observation);
    }
    report.hash_bytes = options.maximum_hash_bytes - budget.remaining;
    let mut seen = HashSet::new();
    match root.entries() {
        Ok(mut entries) => loop {
            if let Err(code) = check(options) {
                report.issue(options, code, None);
                break;
            }
            match entries.next() {
                Ok(None) => {
                    report.inventory_complete = true;
                    break;
                }
                Ok(Some(name)) => {
                    if report.files_enumerated >= options.maximum_files {
                        report.issue(options, "inspection_file_limit", None);
                        break;
                    }
                    report.files_enumerated += 1;
                    let Some(name) = name.to_str() else {
                        report.issue(options, "unrecognized_store_entry", None);
                        charged = None;
                        continue;
                    };
                    if name == MANIFEST {
                        continue;
                    }
                    if name == ".juiced-store.lock" {
                        if report.files_checked >= options.maximum_files {
                            report.issue(options, "inspection_file_limit", None);
                            checks_complete = false;
                            continue;
                        }
                        if root
                            .open_file(name)
                            .and_then(|file| {
                                let stamp = FileStamp::read(&file, false)?;
                                root.verify_file(name, &file, &stamp)
                            })
                            .is_err()
                        {
                            report.issue(options, "control_file_unavailable_or_invalid", None);
                            checks_complete = false;
                        } else {
                            report.files_checked += 1;
                        }
                        continue;
                    }
                    if super::writer::recovery_journal_name(name) {
                        if !journals.contains_key(name) {
                            report.issue(options, "recovery_journal_changed_or_unverified", None);
                            charged = None;
                            checks_complete = false;
                        }
                        continue;
                    }
                    if let Some(id) = snapshot_id(name) {
                        if manifest.entries.contains_key(id) {
                            seen.insert(id.to_owned());
                            continue;
                        }
                        report.issue(options, "orphan_snapshot_file", Some(name));
                        if report.files_checked >= options.maximum_files {
                            report.issue(options, "inspection_file_limit", None);
                            charged = None;
                            checks_complete = false;
                            continue;
                        }
                        match root.open_file(name).and_then(|file| {
                            let stamp = FileStamp::read(&file, false)?;
                            root.verify_file(name, &file, &stamp)?;
                            Ok(stamp.bytes)
                        }) {
                            Ok(bytes) => {
                                report.files_checked += 1;
                                charged = charged.and_then(|total| total.checked_add(bytes));
                            }
                            Err(_) => {
                                report.issue(
                                    options,
                                    "snapshot_file_unavailable_or_invalid",
                                    Some(name),
                                );
                                charged = None;
                                checks_complete = false;
                            }
                        }
                    } else {
                        report.issue(options, "unrecognized_store_entry", Some(name));
                        charged = None;
                    }
                }
                Err(_) => {
                    report.issue(options, "inventory_unreadable", None);
                    break;
                }
            }
        },
        Err(_) => report.issue(options, "inventory_unreadable", None),
    }
    if !report.inventory_complete || report.records_checked != manifest.entries.len() {
        charged = None;
        checks_complete = false;
    }
    for (id, record) in &manifest.entries {
        if record.state == SnapshotState::Saved && !seen.contains(id) && report.inventory_complete {
            checks_complete = false;
        }
    }
    report.charged_bytes = charged;
    if report.charged_bytes.is_none() {
        report.issue(options, "charged_bytes_unverified", None);
    }
    report.validation_complete = checks_complete
        && report.inventory_complete
        && report.charged_bytes.is_some()
        && report.issues.is_empty()
        && !report.issues_truncated;
    if let Err(code) = check(options) {
        report.issue(options, code, None);
        report.invalidate();
    }
    for (name, (file, stamp)) in &journals {
        if root.verify_file(name, file, stamp).is_err() {
            report.issue(options, "recovery_journal_changed_or_unverified", None);
            report.invalidate();
        }
    }
    if root.verify_file(MANIFEST, &manifest_file, &before).is_err() {
        report.issue(options, "manifest_changed_or_unverified", Some(MANIFEST));
        report.invalidate();
    }
    if root.verify().is_err() {
        report.issue(options, "root_changed_or_unverified", None);
        report.root_verified = false;
        report.invalidate();
    }
    if report.validation_complete && !report.issues_truncated {
        report.status = SnapshotInspectionStatus::Complete;
    }
    report
}

fn inspect_recovery_journals(
    root: &DirectoryInput,
    manifest: &mut super::Manifest,
    options: &SnapshotInspectionOptions,
    report: &mut SnapshotStoreInspection,
) -> Result<std::collections::BTreeMap<String, (File, FileStamp)>, &'static str> {
    let mut journals = std::collections::BTreeMap::new();
    let mut entries = root.entries().map_err(|_| "inventory_unreadable")?;
    let mut enumerated = 0usize;
    loop {
        check(options)?;
        let Some(name) = entries.next().map_err(|_| "inventory_unreadable")? else {
            break;
        };
        if enumerated >= options.maximum_files {
            return Err("inspection_file_limit");
        }
        enumerated += 1;
        let Some(name) = name
            .to_str()
            .filter(|name| super::writer::recovery_journal_name(name))
        else {
            continue;
        };
        if journals.len() >= options.maximum_records
            || report.files_checked >= options.maximum_files
        {
            return Err("inspection_file_limit");
        }
        let mut file = root
            .open_file(name)
            .map_err(|_| "recovery_journal_unavailable_or_invalid")?;
        let stamp =
            FileStamp::read(&file, false).map_err(|_| "recovery_journal_unavailable_or_invalid")?;
        if stamp.bytes > super::writer::maximum_recovery_journal_bytes()
            || stamp.bytes
                > options
                    .maximum_manifest_bytes
                    .saturating_sub(report.journal_bytes)
        {
            return Err("recovery_journal_size_limit");
        }
        root.verify_file(name, &file, &stamp)
            .map_err(|_| "recovery_journal_changed_or_unverified")?;
        let mut bytes = vec![0; stamp.bytes as usize];
        check(options)?;
        file.read_exact(&mut bytes)
            .map_err(|_| "recovery_journal_unavailable_or_invalid")?;
        report.journal_bytes += stamp.bytes;
        check(options)?;
        root.verify_file(name, &file, &stamp)
            .map_err(|_| "recovery_journal_changed_or_unverified")?;
        super::writer::decode_recovery_journal(
            &bytes,
            name,
            manifest,
            options.maximum_records,
            options.maximum_snapshot_bytes,
        )
        .map_err(|_| "recovery_journal_invalid")?;
        report.files_checked += 1;
        report.journals_checked += 1;
        report.issue(options, "unresolved_recovery_journal", None);
        journals.insert(name.to_owned(), (file, stamp));
    }
    Ok(journals)
}

fn snapshot_id(name: &str) -> Option<&str> {
    name.strip_prefix("kv-")
        .and_then(|value| value.strip_suffix(".bin"))
        .filter(|id| validate_id(id).is_ok())
}

#[derive(Eq, PartialEq)]
struct FileStamp {
    bytes: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    changed: i64,
}

impl FileStamp {
    fn read(file: &File, directory: bool) -> io::Result<Self> {
        let metadata = file.metadata()?;
        check_type(&metadata, directory)?;
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            changed: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ctime(), metadata.ctime_nsec())
            },
            #[cfg(windows)]
            changed: windows_change_time(file, directory)?,
        })
    }
}

fn check_type(metadata: &Metadata, directory: bool) -> io::Result<()> {
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(io::Error::other("Invalid inspection input type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !directory && metadata.nlink() != 1 {
            return Err(io::Error::other(
                "External hard links are not inspection inputs",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("Reparse points are not inspection inputs"));
        }
    }
    Ok(())
}

struct DirectoryInput {
    path: PathBuf,
    directory: File,
    before: FileStamp,
    ancestors: Vec<(PathBuf, File)>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl DirectoryInput {
    fn open(path: &Path, options: &SnapshotInspectionOptions) -> io::Result<Self> {
        if path.as_os_str().len() > 32768 || path.components().count() > 128 {
            return Err(io::Error::other("Inspection root exceeds path bounds"));
        }
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        if absolute.as_os_str().len() > 32768 || absolute.components().count() > 128 {
            return Err(io::Error::other("Inspection root exceeds path bounds"));
        }
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                    normalized.push(component)
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(io::Error::other("Traversal is not an inspection root"));
                }
            }
        }
        if normalized.parent().is_none() {
            return Err(io::Error::other(
                "A filesystem root is not a snapshot store",
            ));
        }
        let (directory, ancestors) = open_ancestry(&normalized, &|| inspection_io_check(options))?;
        let before = FileStamp::read(&directory, true)?;
        let result = Self {
            path: normalized,
            directory,
            before,
            ancestors,
            deadline: options.deadline,
            cancelled: options.cancelled.clone(),
        };
        result.verify()?;
        Ok(result)
    }

    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Inspection stopped",
            ))
        } else {
            Ok(())
        }
    }

    fn verify(&self) -> io::Result<()> {
        self.check()?;
        for (path, held) in &self.ancestors {
            self.check()?;
            check_type(&fs::symlink_metadata(path)?, true)?;
            if !same_file(held, path)? {
                return Err(io::Error::other("Inspection ancestry changed"));
            }
        }
        check_type(&fs::symlink_metadata(&self.path)?, true)?;
        if !same_file(&self.directory, &self.path)?
            || FileStamp::read(&self.directory, true)? != self.before
        {
            return Err(io::Error::other("Inspection directory changed"));
        }
        Ok(())
    }

    fn open_file(&self, name: &str) -> io::Result<File> {
        self.check()?;
        if name != MANIFEST
            && name != ".juiced-store.lock"
            && snapshot_id(name).is_none()
            && !super::writer::journal_name_valid(name)
        {
            return Err(io::Error::other("Invalid inspection file name"));
        }
        let file = open_leaf(self, name)?;
        check_type(&file.metadata()?, false)?;
        Ok(file)
    }

    fn verify_file(&self, name: &str, file: &File, before: &FileStamp) -> io::Result<()> {
        self.check()?;
        if FileStamp::read(file, false)? != *before {
            return Err(io::Error::other("Inspection file changed"));
        }
        let current = self.open_file(name)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = file.metadata()?;
            let current = current.metadata()?;
            if held.dev() != current.dev() || held.ino() != current.ino() {
                return Err(io::Error::other("Inspection file identity changed"));
            }
        }
        #[cfg(windows)]
        {
            let _retained = current;
            if !same_file(file, &self.path.join(name))? {
                return Err(io::Error::other("Inspection file identity changed"));
            }
        }
        #[cfg(not(any(unix, windows)))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unsupported inspection platform",
        ));
        Ok(())
    }

    fn entries(&self) -> io::Result<DirectoryEntries> {
        DirectoryEntries::open(self)
    }
}

#[cfg(unix)]
fn open_ancestry(
    path: &Path,
    check: &dyn Fn() -> io::Result<()>,
) -> io::Result<(File, Vec<(PathBuf, File)>)> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    check()?;
    let mut directory = OpenOptions::new().read(true).open("/")?;
    let mut ancestors = Vec::new();
    let mut current = PathBuf::from("/");
    for component in path.components().filter_map(|component| match component {
        Component::Normal(name) => Some(name),
        _ => None,
    }) {
        check()?;
        let name = std::ffi::CString::new(component.as_bytes())
            .map_err(|_| io::Error::other("Invalid inspection directory name"))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        ancestors.push((current.clone(), directory));
        current.push(component);
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok((directory, ancestors))
}

#[cfg(unix)]
fn open_leaf(root: &DirectoryInput, name: &str) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name =
        std::ffi::CString::new(name).map_err(|_| io::Error::other("Invalid inspection name"))?;
    let fd = unsafe {
        libc::openat(
            root.directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
struct DirectoryEntries(*mut libc::DIR);

#[cfg(unix)]
impl DirectoryEntries {
    fn open(root: &DirectoryInput) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        if errno_pointer().is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Directory inspection is unsupported",
            ));
        }
        let fd = unsafe {
            libc::openat(
                root.directory.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let directory = unsafe { libc::fdopendir(fd) };
        if directory.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        Ok(Self(directory))
    }

    fn next(&mut self) -> io::Result<Option<OsString>> {
        use std::os::unix::ffi::OsStrExt;
        loop {
            unsafe {
                *errno_pointer() = 0;
            }
            let entry = unsafe { libc::readdir(self.0) };
            if entry.is_null() {
                let error = unsafe { *errno_pointer() };
                return if error == 0 {
                    Ok(None)
                } else {
                    Err(io::Error::from_raw_os_error(error))
                };
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            return Ok(Some(std::ffi::OsStr::from_bytes(name).to_owned()));
        }
    }
}

#[cfg(unix)]
impl Drop for DirectoryEntries {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(unix)]
fn errno_pointer() -> *mut libc::c_int {
    #[cfg(target_os = "linux")]
    return unsafe { libc::__errno_location() };
    #[cfg(target_os = "android")]
    return unsafe { libc::__errno() };
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    return unsafe { libc::__error() };
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    std::ptr::null_mut()
}

#[cfg(windows)]
fn open_ancestry(
    path: &Path,
    check: &dyn Fn() -> io::Result<()>,
) -> io::Result<(File, Vec<(PathBuf, File)>)> {
    use std::os::windows::fs::OpenOptionsExt;
    let components: Vec<_> = path.components().collect();
    if !matches!(components.first(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(io::Error::other(
            "Only drive-rooted Windows inspection paths are supported",
        ));
    }
    let mut ancestors = Vec::new();
    let mut current = PathBuf::new();
    let mut directory = None;
    for component in components {
        check()?;
        current.push(component);
        if !current.has_root() {
            continue;
        }
        let file = OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(0x02000000 | 0x00200000)
            .open(&current)?;
        check_type(&file.metadata()?, true)?;
        if let Some(previous) = directory.take() {
            ancestors.push((current.parent().unwrap_or(&current).to_owned(), previous));
        }
        directory = Some(file);
    }
    directory
        .map(|directory| (directory, ancestors))
        .ok_or_else(|| io::Error::other("Invalid inspection root"))
}

#[cfg(windows)]
fn open_leaf(root: &DirectoryInput, name: &str) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .share_mode(7)
        .custom_flags(0x00200000 | 0x02000000)
        .open(root.path.join(name))
}

#[cfg(windows)]
struct DirectoryEntries(fs::ReadDir);

#[cfg(windows)]
impl DirectoryEntries {
    fn open(root: &DirectoryInput) -> io::Result<Self> {
        root.verify()?;
        Ok(Self(fs::read_dir(&root.path)?))
    }
    fn next(&mut self) -> io::Result<Option<OsString>> {
        self.0
            .next()
            .transpose()
            .map(|entry| entry.map(|entry| entry.file_name()))
    }
}

#[cfg(windows)]
fn windows_change_time(file: &File, directory: bool) -> io::Result<i64> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct BasicInfo {
        creation: i64,
        accessed: i64,
        modified: i64,
        changed: i64,
        attributes: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct StandardInfo {
        allocated: i64,
        bytes: i64,
        links: u32,
        delete_pending: u8,
        directory: u8,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: *mut std::ffi::c_void,
            class: i32,
            information: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    let mut basic = BasicInfo::default();
    let mut standard = StandardInfo::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            0,
            std::ptr::from_mut(&mut basic).cast(),
            std::mem::size_of::<BasicInfo>() as u32,
        )
    } == 0
        || unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                1,
                std::ptr::from_mut(&mut standard).cast(),
                std::mem::size_of::<StandardInfo>() as u32,
            )
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if (!directory && standard.links != 1)
        || (standard.directory != 0) != directory
        || basic.attributes & 0x400 != 0
    {
        return Err(io::Error::other("Invalid retained inspection input"));
    }
    Ok(basic.changed)
}

#[cfg(not(any(unix, windows)))]
fn open_ancestry(
    _path: &Path,
    _check: &dyn Fn() -> io::Result<()>,
) -> io::Result<(File, Vec<(PathBuf, File)>)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unsupported inspection platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn open_leaf(_root: &DirectoryInput, _name: &str) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unsupported inspection platform",
    ))
}

#[cfg(not(any(unix, windows)))]
struct DirectoryEntries;

#[cfg(not(any(unix, windows)))]
impl DirectoryEntries {
    fn open(_root: &DirectoryInput) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unsupported inspection platform",
        ))
    }
    fn next(&mut self) -> io::Result<Option<OsString>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cache::{CacheStore, SnapshotIdentity},
        storage::DirectoryLease,
    };
    use std::collections::BTreeMap;

    fn identity() -> SnapshotIdentity {
        SnapshotIdentity {
            execution_fingerprint: "a".repeat(64),
            principal: "private-principal-do-not-report".into(),
            credential_epoch: "private-epoch-do-not-report".into(),
            protocol: "openai-completions".into(),
            session_digest: "b".repeat(64),
        }
    }

    fn fixture() -> (tempfile::TempDir, Arc<CacheStore>) {
        let temporary = tempfile::tempdir().unwrap();
        let store =
            CacheStore::open(DirectoryLease::acquire(temporary.path().join("cache")).unwrap())
                .unwrap();
        (temporary, store)
    }

    fn save(store: &Arc<CacheStore>, bytes: &[u8]) -> String {
        let pending = store
            .begin(identity(), store.generation(), 1024 * 1024)
            .unwrap();
        fs::write(pending.path(), bytes).unwrap();
        store.publish(&pending, pending.revision()).unwrap().id
    }

    fn tree(root: &Path) -> BTreeMap<OsString, Vec<u8>> {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    fn has(report: &SnapshotStoreInspection, code: &str) -> bool {
        report.issues.iter().any(|issue| issue.code == code)
    }

    #[test]
    fn missing_empty_and_legacy_roots_have_no_creation_or_recovery_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing/child");
        let options = SnapshotInspectionOptions::default();
        let report = inspect_snapshot_store(&missing, &options);
        assert_eq!(report.status, SnapshotInspectionStatus::Unknown);
        assert!(has(&report, "root_unavailable_or_invalid"));
        assert!(!temporary.path().join("missing").exists());
        assert!(
            !report.application_authorized
                && !report.writer_termination_verified
                && !report.recovery_authorized
        );
        let empty = temporary.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let report = inspect_snapshot_store(&empty, &options);
        assert!(has(&report, "manifest_unavailable_or_invalid"));
        assert!(tree(&empty).is_empty());
        fs::write(empty.join(MANIFEST), br#"{"native_state":{},"saved":{}}"#).unwrap();
        let before = tree(&empty);
        let report = inspect_snapshot_store(&empty, &options);
        assert!(has(&report, "manifest_invalid"));
        assert!(!report.validation_complete);
        assert_eq!(before, tree(&empty));
        assert!(!empty.join(".juiced-store.lock").exists());
    }

    #[test]
    fn healthy_inspection_coexists_with_writer_and_preserves_files_and_charges() {
        let (temporary, store) = fixture();
        let payload = vec![42; 180_000];
        let id = save(&store, &payload);
        let root = temporary.path().join("cache");
        let before = tree(&root);
        let charge = store.inspect().unwrap().charged_bytes;
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        assert_eq!(report.status, SnapshotInspectionStatus::Complete);
        assert!(
            report.validation_complete
                && report.root_verified
                && report.manifest_verified
                && report.inventory_complete
        );
        assert!(report.issues.is_empty());
        assert!(
            report.read_only
                && report.non_atomic
                && report.no_recovery_authority
                && report.cooperative_deadline
        );
        assert!(
            !report.application_authorized
                && !report.writer_termination_verified
                && !report.recovery_authorized
        );
        assert_eq!(report.records_checked, 1);
        assert_eq!(report.hash_bytes, payload.len() as u64);
        assert_eq!(report.snapshots[0].id, id);
        assert_eq!(report.snapshots[0].checksum_valid, Some(true));
        assert_eq!(report.charged_bytes, Some(charge));
        assert_eq!(tree(&root), before);
        assert_eq!(store.inspect().unwrap().charged_bytes, charge);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("private-principal") && !json.contains("private-epoch"));
        assert!(!json.contains(&root.to_string_lossy().to_string()));
    }

    #[test]
    fn pending_and_corrupt_snapshots_stay_charged_without_cleanup() {
        let (temporary, store) = fixture();
        let id = save(&store, b"original");
        let root = temporary.path().join("cache");
        fs::write(root.join(format!("kv-{id}.bin")), b"modified").unwrap();
        let pending = store.begin(identity(), store.generation(), 512).unwrap();
        fs::write(pending.path(), b"partial").unwrap();
        drop(pending);
        let before = tree(&root);
        let charge = store.inspect().unwrap().charged_bytes;
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        assert!(has(&report, "snapshot_checksum_mismatch"));
        assert!(has(&report, "unresolved_snapshot_operation"));
        assert!(!report.validation_complete);
        assert!(
            report
                .snapshots
                .iter()
                .any(|snapshot| snapshot.checksum_valid == Some(false))
        );
        assert_eq!(report.charged_bytes, Some(charge));
        assert_eq!(tree(&root), before);
        assert_eq!(store.inspect().unwrap().charged_bytes, charge);
    }

    #[test]
    fn manifest_and_record_limits_reject_before_publishing_observations() {
        let (temporary, store) = fixture();
        save(&store, b"first");
        save(&store, b"second");
        let root = temporary.path().join("cache");
        let before = tree(&root);
        let options = SnapshotInspectionOptions {
            maximum_manifest_bytes: 32,
            ..Default::default()
        };
        let report = inspect_snapshot_store(&root, &options);
        assert!(has(&report, "manifest_size_limit"));
        assert_eq!(report.manifest_bytes, 0);
        let options = SnapshotInspectionOptions {
            maximum_records: 1,
            ..Default::default()
        };
        let report = inspect_snapshot_store(&root, &options);
        assert!(has(&report, "manifest_invalid"));
        assert_eq!(report.records_checked, 0);
        assert!(report.snapshots.is_empty());
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn hash_and_file_budgets_leave_unchecked_results_unknown() {
        let (temporary, store) = fixture();
        save(&store, &vec![7; 70_000]);
        let root = temporary.path().join("cache");
        let before = tree(&root);
        let report = inspect_snapshot_store(
            &root,
            &SnapshotInspectionOptions {
                maximum_hash_bytes: 69_999,
                ..Default::default()
            },
        );
        assert!(has(&report, "inspection_hash_limit"));
        assert!(!report.validation_complete);
        assert_eq!(report.hash_bytes, 0);
        assert_eq!(report.snapshots[0].checksum_valid, None);
        let report = inspect_snapshot_store(
            &root,
            &SnapshotInspectionOptions {
                maximum_files: 1,
                ..Default::default()
            },
        );
        assert!(has(&report, "inspection_file_limit"));
        assert!(!report.validation_complete);
        assert!(report.files_enumerated <= 1 && report.files_checked <= 1);
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn total_hash_budget_is_shared_across_files() {
        let (temporary, store) = fixture();
        save(&store, &vec![1; 20_000]);
        save(&store, &vec![2; 20_000]);
        let root = temporary.path().join("cache");
        let before = tree(&root);
        let report = inspect_snapshot_store(
            &root,
            &SnapshotInspectionOptions {
                maximum_hash_bytes: 30_000,
                ..Default::default()
            },
        );
        assert_eq!(report.records_checked, 2);
        assert_eq!(report.hash_bytes, 20_000);
        assert_eq!(
            report
                .snapshots
                .iter()
                .filter(|snapshot| snapshot.checksum_valid == Some(true))
                .count(),
            1
        );
        assert_eq!(
            report
                .snapshots
                .iter()
                .filter(|snapshot| snapshot.checksum_valid.is_none())
                .count(),
            1
        );
        assert!(!report.validation_complete);
        assert!(has(&report, "inspection_hash_limit"));
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn cancellation_and_deadline_stop_without_opening_or_mutating_the_store() {
        let (temporary, store) = fixture();
        save(&store, b"saved");
        let root = temporary.path().join("cache");
        let before = tree(&root);
        let options = SnapshotInspectionOptions::default();
        options.cancelled.store(true, Ordering::Relaxed);
        let report = inspect_snapshot_store(&root, &options);
        assert!(has(&report, "inspection_cancelled"));
        assert_eq!(report.hash_bytes, 0);
        assert_eq!(report.manifest_bytes, 0);
        let report = inspect_snapshot_store(
            &root,
            &SnapshotInspectionOptions {
                deadline: Instant::now(),
                ..Default::default()
            },
        );
        assert!(has(&report, "inspection_deadline"));
        assert!(!report.validation_complete);
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn issue_and_output_paths_are_bounded_and_redacted() {
        let (temporary, store) = fixture();
        let _keep_writer = store;
        let root = temporary.path().join("cache");
        for number in 0..20 {
            fs::write(
                root.join(format!("private-user-secret-{number}")),
                b"unknown",
            )
            .unwrap();
        }
        let before = tree(&root);
        let report = inspect_snapshot_store(
            &root,
            &SnapshotInspectionOptions {
                maximum_issues: 2,
                ..Default::default()
            },
        );
        assert_eq!(report.issues.len(), 2);
        assert!(report.issues_truncated);
        assert_ne!(report.status, SnapshotInspectionStatus::Complete);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-user-secret")
        );
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn malformed_duplicate_and_traversal_manifest_records_are_rejected_by_shared_decoder() {
        let (temporary, store) = fixture();
        let _keep_writer = store;
        let root = temporary.path().join("cache");
        for invalid in [
            "{",
            r#"{"schema_version":1,"schema_version":1,"generation":0,"entries":{}}"#,
            r#"{"schema_version":1,"generation":0,"entries":{"../../outside":{}}}"#,
            r#"{"schema_version":1,"generation":0,"entries":{}} trailing"#,
        ] {
            fs::write(root.join(MANIFEST), invalid).unwrap();
            let before = tree(&root);
            let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
            assert!(has(&report, "manifest_invalid"));
            assert_eq!(report.hash_bytes, 0);
            assert_eq!(tree(&root), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_ancestor_manifest_and_snapshot_links_never_hash_external_bytes() {
        use std::os::unix::fs::symlink;
        let (temporary, store) = fixture();
        let id = save(&store, b"snapshot");
        let root = temporary.path().join("cache");
        let external = temporary.path().join("external-secret");
        fs::write(&external, b"outside-data").unwrap();
        let original = fs::read(&external).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&root, &alias).unwrap();
        let report = inspect_snapshot_store(&alias, &SnapshotInspectionOptions::default());
        assert!(has(&report, "root_unavailable_or_invalid"));
        let report =
            inspect_snapshot_store(alias.join("nested"), &SnapshotInspectionOptions::default());
        assert!(has(&report, "root_unavailable_or_invalid"));
        fs::remove_file(root.join(format!("kv-{id}.bin"))).unwrap();
        symlink(&external, root.join(format!("kv-{id}.bin"))).unwrap();
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        assert!(has(&report, "snapshot_file_unavailable_or_invalid"));
        assert_eq!(report.hash_bytes, 0);
        fs::remove_file(root.join(MANIFEST)).unwrap();
        symlink(&external, root.join(MANIFEST)).unwrap();
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        assert!(has(&report, "manifest_unavailable_or_invalid"));
        assert_eq!(report.manifest_bytes, 0);
        assert_eq!(fs::read(&external).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn retained_root_replacement_is_unknown_and_does_not_touch_new_root_or_external_data() {
        let (temporary, store) = fixture();
        let _keep_writer = store;
        let root = temporary.path().join("cache");
        let anchor = DirectoryInput::open(&root, &SnapshotInspectionOptions::default()).unwrap();
        let retained_manifest = fs::read(root.join(MANIFEST)).unwrap();
        let old = temporary.path().join("retired");
        fs::rename(&root, &old).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("external-secret"), b"external-data").unwrap();
        fs::write(root.join(MANIFEST), b"external-manifest-do-not-read").unwrap();
        let new_before = tree(&root);
        let old_before = tree(&old);
        let mut anchored_bytes = Vec::new();
        anchor
            .open_file(MANIFEST)
            .unwrap()
            .read_to_end(&mut anchored_bytes)
            .unwrap();
        assert_eq!(anchored_bytes, retained_manifest);
        let report = inspect_directory(
            anchor,
            &SnapshotInspectionOptions::default(),
            SnapshotStoreInspection::new(),
        );
        assert_eq!(report.status, SnapshotInspectionStatus::Unknown);
        assert!(has(&report, "root_changed_or_unverified"));
        assert!(!report.root_verified && !report.manifest_verified);
        assert_eq!(report.hash_bytes, 0);
        assert_eq!(report.manifest_bytes, 0);
        assert_eq!(tree(&root), new_before);
        assert_eq!(tree(&old), old_before);
    }

    #[cfg(unix)]
    #[test]
    fn retained_manifest_replacement_and_same_size_content_changes_are_unverified() {
        let (temporary, store) = fixture();
        let id = save(&store, b"original");
        let root = temporary.path().join("cache");
        let anchor = DirectoryInput::open(&root, &SnapshotInspectionOptions::default()).unwrap();
        let manifest = anchor.open_file(MANIFEST).unwrap();
        let manifest_stamp = FileStamp::read(&manifest, false).unwrap();
        let bytes = fs::read(root.join(MANIFEST)).unwrap();
        fs::rename(root.join(MANIFEST), root.join("retired-manifest")).unwrap();
        fs::write(root.join(MANIFEST), bytes).unwrap();
        assert!(
            anchor
                .verify_file(MANIFEST, &manifest, &manifest_stamp)
                .is_err()
        );
        let name = format!("kv-{id}.bin");
        let file = anchor.open_file(&name).unwrap();
        let stamp = FileStamp::read(&file, false).unwrap();
        fs::write(root.join(&name), b"modified").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(stamp.modified))
            .unwrap();
        let after = FileStamp::read(&file, false).unwrap();
        assert_eq!(stamp.bytes, after.bytes);
        assert_eq!(stamp.modified, after.modified);
        assert!(anchor.verify_file(&name, &file, &stamp).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn read_only_store_without_a_control_file_is_inspectable_without_writer_claim() {
        use std::os::unix::fs::PermissionsExt;
        let (temporary, store) = fixture();
        let id = save(&store, b"readonly");
        let source = temporary.path().join("cache");
        let root = temporary.path().join("readonly");
        fs::create_dir(&root).unwrap();
        for name in [MANIFEST.to_owned(), format!("kv-{id}.bin")] {
            fs::copy(source.join(&name), root.join(&name)).unwrap();
            fs::set_permissions(root.join(&name), fs::Permissions::from_mode(0o444)).unwrap();
        }
        let before = tree(&root);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(report.validation_complete);
        assert!(report.issues.is_empty());
        assert_eq!(tree(&root), before);
        assert!(!root.join(".juiced-store.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_and_special_inputs_are_rejected_without_reading_or_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let (temporary, store) = fixture();
        let id = save(&store, b"snapshot");
        let root = temporary.path().join("cache");
        let path = root.join(format!("kv-{id}.bin"));
        let external = temporary.path().join("hard-link");
        fs::hard_link(&path, &external).unwrap();
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        assert!(has(&report, "snapshot_file_unavailable_or_invalid"));
        assert_eq!(report.hash_bytes, 0);
        fs::remove_file(root.join(MANIFEST)).unwrap();
        let name = std::ffi::CString::new(root.join(MANIFEST).as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let report = inspect_snapshot_store(&root, &SnapshotInspectionOptions::default());
        assert!(has(&report, "manifest_unavailable_or_invalid"));
        assert_eq!(report.manifest_bytes, 0);
    }
}
