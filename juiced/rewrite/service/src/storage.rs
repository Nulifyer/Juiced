use serde::Serialize;
mod json_size;
pub(crate) use json_size::{JsonSizeError, check_json_size, json_size};
mod metadata;
mod private;
#[cfg(unix)]
pub(crate) mod runtime_payload;
pub use metadata::{MetadataCopy, MetadataFile, MetadataObservation};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

pub(crate) const CONTROL_FILE: &str = ".juiced-store.lock";
const MAXIMUM_READ_BYTES: u64 = 16 * 1024 * 1024;
const MAXIMUM_LIVE_ROOTS: usize = 64;

pub fn generated_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    let mut id = String::with_capacity(32);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        id.push(HEX[(byte >> 4) as usize] as char);
        id.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(id)
}

#[derive(Clone)]
pub struct DirectoryLease {
    owner: Arc<Owner>,
    claim: Arc<Claim>,
}

#[derive(Clone)]
pub struct DirectoryReader {
    inner: Arc<ReadDirectory>,
}

struct ReadDirectory {
    root: PathBuf,
    directories: Box<[(PathBuf, File)]>,
}

pub struct ReadObservation {
    reader: DirectoryReader,
    filename: PathBuf,
    content: Option<SourceContent>,
}

struct SourceContent {
    bytes: Arc<Vec<u8>>,
    file: File,
    stamp: ReadStamp,
}

impl From<ReadContent> for SourceContent {
    fn from(content: ReadContent) -> Self {
        Self {
            bytes: Arc::new(content.bytes),
            file: content.file,
            stamp: content.stamp,
        }
    }
}

struct ReadContent {
    bytes: Vec<u8>,
    file: File,
    stamp: ReadStamp,
}

impl DirectoryReader {
    pub fn open_existing(path: impl AsRef<Path>) -> io::Result<Self> {
        if path.as_ref().as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A source directory must be explicit",
            ));
        }
        let root = fs::canonicalize(path)?;
        let directories = root
            .ancestors()
            .map(|path| Ok((path.to_path_buf(), open_directory(path)?)))
            .collect::<io::Result<Vec<_>>>()?
            .into_boxed_slice();
        let reader = Self {
            inner: Arc::new(ReadDirectory { root, directories }),
        };
        reader.verify()?;
        Ok(reader)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn verify(&self) -> io::Result<()> {
        for (path, file) in &self.inner.directories {
            verify_directory(file, path)?;
        }
        Ok(())
    }

    pub fn observe(&self, filename: impl AsRef<Path>, maximum: u64) -> io::Result<ReadObservation> {
        let filename = filename.as_ref();
        let content = match read_child_checked(
            self.directory(),
            filename,
            maximum,
            || self.verify(),
            |_, _| {},
        ) {
            Ok(content) => Some(SourceContent::from(content)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.verify()?;
                None
            }
            Err(error) => return Err(error),
        };
        let observation = ReadObservation {
            reader: self.clone(),
            filename: filename.to_path_buf(),
            content,
        };
        observation.verify()?;
        Ok(observation)
    }

    fn directory(&self) -> &File {
        &self.inner.directories[0].1
    }
}

impl ReadObservation {
    pub fn bytes(&self) -> Option<&[u8]> {
        self.content
            .as_ref()
            .map(|content| content.bytes.as_slice())
    }

    pub fn shared_bytes(&self) -> Option<Arc<Vec<u8>>> {
        self.content.as_ref().map(|content| content.bytes.clone())
    }

    pub fn permissions(&self) -> io::Result<Option<fs::Permissions>> {
        self.verify()?;
        self.content
            .as_ref()
            .map(|content| {
                content
                    .file
                    .metadata()
                    .map(|metadata| metadata.permissions())
            })
            .transpose()
    }

    pub fn verify(&self) -> io::Result<()> {
        self.reader.verify()?;
        match (
            &self.content,
            open_read_child(self.reader.directory(), &self.filename),
        ) {
            (Some(content), Ok(current))
                if same_handles(&content.file, &current)?
                    && ReadStamp::read(&content.file)? == content.stamp
                    && ReadStamp::read(&current)? == content.stamp => {}
            (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
            (_, Err(error)) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => return Err(read_changed()),
        }
        self.reader.verify()
    }
}

pub struct LiveRoots {
    roots: Box<[DirectoryLease]>,
    observations: Box<[Observation]>,
}

enum Observation {
    Claim(Arc<Claim>),
    #[cfg(unix)]
    Shared(Arc<DirectoryLock>),
}

impl Observation {
    fn verify(&self) -> io::Result<()> {
        match self {
            Self::Claim(claim) => claim.verify_shallow(),
            #[cfg(unix)]
            Self::Shared(lock) => verify_directory(&lock.file, &lock.path),
        }
    }
}

fn retained_observations(roots: &[DirectoryLease]) -> Box<[Observation]> {
    let mut claims = HashSet::new();
    #[cfg(unix)]
    let mut shared = HashSet::new();
    let mut pending: Vec<_> = roots
        .iter()
        .rev()
        .map(|lease| Observation::Claim(lease.claim.clone()))
        .collect();
    let mut observations = Vec::new();
    while let Some(observation) = pending.pop() {
        match &observation {
            Observation::Claim(claim) => {
                if !claims.insert(Arc::as_ptr(claim)) {
                    continue;
                }
                #[cfg(unix)]
                pending.extend(claim.ancestors.iter().rev().map(|ancestor| match ancestor {
                    Ancestor::Owned(claim) => Observation::Claim(claim.clone()),
                    Ancestor::Shared(lock) => Observation::Shared(lock.clone()),
                }));
            }
            #[cfg(unix)]
            Observation::Shared(lock) => {
                if !shared.insert(Arc::as_ptr(lock)) {
                    continue;
                }
            }
        }
        observations.push(observation);
    }
    observations.into_boxed_slice()
}

fn read_child_checked(
    directory: &File,
    filename: &Path,
    maximum: u64,
    verify: impl Fn() -> io::Result<()>,
    mut checkpoint: impl FnMut(ReadPoint, &File),
) -> io::Result<ReadContent> {
    if maximum == 0 || maximum > MAXIMUM_READ_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Managed reads require a limit between 1 byte and 16 MiB",
        ));
    }
    validate_filename(filename)?;
    verify()?;
    checkpoint(ReadPoint::BeforeOpen, directory);
    let mut file = open_read_child(directory, filename)?;
    let before = ReadStamp::read(&file)?;
    if before.bytes > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The managed file exceeds its read limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(before.bytes as usize)
        .map_err(|_| io::Error::other("The managed read buffer could not be allocated"))?;
    checkpoint(ReadPoint::AfterOpen, &file);
    let mut buffer = [0u8; 65536];
    while (bytes.len() as u64) < before.bytes {
        let request = (before.bytes - bytes.len() as u64).min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..request])?;
        if count == 0 {
            return Err(read_changed());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    checkpoint(ReadPoint::AfterRead, &file);
    if ReadStamp::read(&file)? != before {
        return Err(read_changed());
    }
    let current = open_read_child(directory, filename)?;
    if !same_handles(&file, &current)? || ReadStamp::read(&current)? != before {
        return Err(read_changed());
    }
    verify()?;
    Ok(ReadContent {
        bytes,
        file,
        stamp: before,
    })
}

#[derive(Debug)]
pub struct RootUnavailable {
    source: io::Error,
}

impl std::fmt::Display for RootUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("A managed storage root is unavailable")
    }
}

impl std::error::Error for RootUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<RootUnavailable> for io::Error {
    fn from(error: RootUnavailable) -> Self {
        Self::other(error)
    }
}

pub fn is_root_unavailable(error: &io::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = current {
        if error.is::<RootUnavailable>() {
            return true;
        }
        if let Some(error) = error.downcast_ref::<io::Error>()
            && let Some(inner) = error.get_ref()
            && inner.is::<RootUnavailable>()
        {
            return true;
        }
        current = error.source();
    }
    false
}

impl LiveRoots {
    pub fn new(mut roots: Vec<DirectoryLease>) -> io::Result<Arc<Self>> {
        if roots.is_empty() || roots.len() > MAXIMUM_LIVE_ROOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A storage scope requires between 1 and 64 roots",
            ));
        }
        roots.sort_by(|left, right| left.root().cmp(right.root()));
        roots.dedup_by(|left, right| Arc::ptr_eq(&left.claim, &right.claim));
        let observations = retained_observations(&roots);
        let scope = Arc::new(Self {
            roots: roots.into_boxed_slice(),
            observations,
        });
        scope.verify()?;
        Ok(scope)
    }

    pub fn single(lease: DirectoryLease) -> io::Result<Arc<Self>> {
        Self::new(vec![lease])
    }
    pub(crate) fn extend(self: &Arc<Self>, lease: DirectoryLease) -> io::Result<Arc<Self>> {
        self.verify()?;
        if self.contains(&lease) {
            return Ok(self.clone());
        }
        let mut roots = self.roots.to_vec();
        roots.push(lease);
        Self::new(roots)
    }

    pub fn contains(&self, lease: &DirectoryLease) -> bool {
        self.roots
            .iter()
            .any(|held| Arc::ptr_eq(&held.claim, &lease.claim))
    }

    pub fn verify(&self) -> Result<(), RootUnavailable> {
        for observation in &self.observations {
            observation
                .verify()
                .map_err(|source| RootUnavailable { source })?;
        }
        Ok(())
    }

    pub fn write_json<T: Serialize + ?Sized>(
        &self,
        lease: &DirectoryLease,
        relative: impl AsRef<Path>,
        value: &T,
    ) -> Result<AtomicCommit, AtomicWriteError> {
        if !self.contains(lease) {
            return Err(AtomicWriteError::new(
                WriteStage::Validate,
                false,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The directory lease is outside this storage scope",
                ),
            ));
        }
        let authority = WriteAuthority::Scope(self);
        authority
            .verify()
            .map_err(|error| AtomicWriteError::new(WriteStage::Validate, false, error))?;
        let path = lease.write_path(relative.as_ref())?;
        atomic_json_checked(&path, value, authority)
    }
}

#[cfg(all(test, target_os = "linux"))]
fn test_conflicting_actors(root: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let mut pids = std::collections::BTreeSet::from([std::process::id()]);
    if let Ok(tasks) = fs::read_dir("/proc/self/task") {
        for task in tasks.take(1024).flatten() {
            if let Ok(children) = fs::read_to_string(task.path().join("children")) {
                pids.extend(
                    children
                        .split_whitespace()
                        .filter_map(|pid| pid.parse::<u32>().ok()),
                );
            }
        }
    }
    let mut holders = Vec::new();
    let mut inodes = std::collections::BTreeSet::new();
    for pid in pids.into_iter().take(128) {
        let process = std::path::PathBuf::from("/proc").join(pid.to_string());
        let executable = fs::read_link(process.join("exe")).ok().and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        if let Ok(fds) = fs::read_dir(process.join("fd")) {
            for fd in fds.take(1024).flatten() {
                if let Ok(target) = fs::read_link(fd.path())
                    && target.starts_with(root)
                {
                    let identity = fs::metadata(fd.path())
                        .ok()
                        .map(|metadata| (metadata.dev(), metadata.ino()));
                    if let Some((_, inode)) = identity {
                        inodes.insert(inode);
                    }
                    holders.push((
                        pid,
                        executable.clone(),
                        fd.file_name().to_string_lossy().into_owned(),
                        target,
                        identity,
                    ));
                }
            }
        }
    }
    let locks: Vec<_> = fs::read_to_string("/proc/locks")
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.split_whitespace()
                .nth(5)
                .and_then(|id| id.rsplit(':').next())
                .and_then(|inode| inode.parse::<u64>().ok())
                .is_some_and(|inode| inodes.contains(&inode))
        })
        .take(128)
        .map(str::to_owned)
        .collect();
    serde_json::json!({"root":root,"holders":holders,"matching_inode_locks":locks}).to_string()
}
#[cfg(all(test, not(target_os = "linux")))]
fn test_conflicting_actors(_: &std::path::Path) -> String {
    "Actor diagnostics unavailable on this platform".into()
}

#[cfg(test)]
// Concurrent forks can retain CLOEXEC lock descriptions until exec.
pub(crate) fn after_test_release<T, E: Into<Box<dyn std::error::Error>>>(
    root: &Path,
    mut acquire: impl FnMut() -> Result<T, E>,
) -> T {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut attempts = 0;
    let mut initial = None;
    loop {
        match acquire() {
            Ok(value) => {
                if attempts != 0 {
                    eprintln!(
                        "test ownership settled after {attempts} refusals; initial actors={}",
                        initial.as_deref().unwrap_or("unavailable")
                    );
                }
                return value;
            }
            Err(error) => {
                let error: Box<dyn std::error::Error> = error.into();
                let mut cause: &(dyn std::error::Error + 'static) = error.as_ref();
                let blocked = loop {
                    if cause
                        .downcast_ref::<io::Error>()
                        .is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock)
                    {
                        break true;
                    }
                    match cause.source() {
                        Some(source) => cause = source,
                        None => break false,
                    }
                };
                if blocked && std::time::Instant::now() < deadline {
                    if initial.is_none() {
                        initial = Some(test_conflicting_actors(root));
                    }
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(2));
                } else {
                    panic!(
                        "test ownership did not settle: {error}; initial actors={initial:?}; current actors={}",
                        test_conflicting_actors(root)
                    );
                }
            }
        }
    }
}

#[derive(Default)]
struct Owner {
    claims: Mutex<Vec<Weak<Claim>>>,
}

struct Claim {
    root: PathBuf,
    directory: File,
    _control: File,
    private: bool,
    #[cfg(unix)]
    ancestors: Vec<Ancestor>,
}

#[cfg(unix)]
enum Ancestor {
    Shared(Arc<DirectoryLock>),
    Owned(Arc<Claim>),
}

#[cfg(unix)]
struct DirectoryLock {
    path: PathBuf,
    file: File,
}

impl DirectoryLease {
    pub fn acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::acquire_owned(Arc::new(Owner::default()), path.as_ref())
    }

    pub fn acquire_many<I, P>(paths: I) -> io::Result<Vec<Self>>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut roots = paths
            .into_iter()
            .map(|path| resolve_root(path.as_ref()))
            .collect::<io::Result<Vec<_>>>()?;
        roots.sort_by(|left, right| {
            left.components()
                .count()
                .cmp(&right.components().count())
                .then_with(|| left.cmp(right))
        });
        let owner = Arc::new(Owner::default());
        roots
            .into_iter()
            .map(|path| Self::acquire_owned(owner.clone(), &path))
            .collect()
    }

    pub fn claim(&self, path: impl AsRef<Path>) -> io::Result<Self> {
        self.verify()?;
        Self::acquire_owned(self.owner.clone(), path.as_ref())
    }

    pub fn root(&self) -> &Path {
        &self.claim.root
    }

    pub fn hierarchy_protected(&self) -> bool {
        cfg!(unix)
    }

    pub fn verify(&self) -> io::Result<()> {
        self.claim
            .verify()
            .map_err(|source| RootUnavailable { source }.into())
    }

    pub fn file_path(&self, filename: impl AsRef<Path>) -> io::Result<PathBuf> {
        let filename = filename.as_ref();
        validate_filename(filename)?;
        self.verify()?;
        Ok(self.root().join(filename))
    }

    pub fn read_bytes(&self, filename: impl AsRef<Path>, maximum: u64) -> io::Result<Vec<u8>> {
        self.read_bytes_checked(filename.as_ref(), maximum, |_, _| {})
    }

    fn read_bytes_checked(
        &self,
        filename: &Path,
        maximum: u64,
        mut checkpoint: impl FnMut(ReadPoint, &File),
    ) -> io::Result<Vec<u8>> {
        read_child_checked(
            &self.claim.directory,
            filename,
            maximum,
            || self.verify(),
            &mut checkpoint,
        )
        .map(|content| content.bytes)
    }

    pub fn write_json<T: Serialize + ?Sized>(
        &self,
        relative: impl AsRef<Path>,
        value: &T,
    ) -> Result<AtomicCommit, AtomicWriteError> {
        let path = self.write_path(relative.as_ref())?;
        atomic_json_checked(&path, value, WriteAuthority::Lease(self))
    }

    fn write_path(&self, relative: &Path) -> Result<PathBuf, AtomicWriteError> {
        self.verify()
            .map_err(|error| AtomicWriteError::new(WriteStage::Validate, false, error))?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AtomicWriteError::new(
                WriteStage::Validate,
                false,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Use a relative file path without traversal",
                ),
            ));
        }
        if let Some(filename) = relative.file_name() {
            validate_leaf(filename)
                .map_err(|error| AtomicWriteError::new(WriteStage::Validate, false, error))?;
        }
        let path = self.root().join(relative);
        let parent = fs::canonicalize(path.parent().expect("managed file has a parent")).map_err(
            |error| WriteAuthority::Lease(self).failure(WriteStage::Validate, false, error),
        )?;
        if !parent.starts_with(self.root()) {
            return Err(AtomicWriteError::new(
                WriteStage::Validate,
                false,
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "The file parent escapes the managed root",
                ),
            ));
        }
        self.verify()
            .map_err(|error| AtomicWriteError::new(WriteStage::Validate, false, error))?;
        Ok(path)
    }

    fn acquire_owned(owner: Arc<Owner>, path: &Path) -> io::Result<Self> {
        let root = resolve_root(path)?;
        if root.parent().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A managed root cannot be a filesystem root",
            ));
        }
        let mut registry = owner
            .claims
            .lock()
            .map_err(|_| io::Error::other("Directory ownership state is unavailable"))?;
        registry.retain(|claim| claim.strong_count() > 0);
        let existing: Vec<_> = registry.iter().filter_map(Weak::upgrade).collect();
        for claim in &existing {
            claim.verify()?;
            if claim.root == root || equivalent(&claim.directory, &root).unwrap_or(false) {
                return Ok(Self {
                    owner: owner.clone(),
                    claim: claim.clone(),
                });
            }
            if claim.root.starts_with(&root) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "A broader root cannot replace live narrower ownership",
                ));
            }
        }

        #[cfg(unix)]
        let ancestors = acquire_ancestors(&root, &existing)?;
        #[cfg(not(unix))]
        {
            if !root.exists() {
                fs::create_dir_all(&root)?;
            }
        }

        let directory = open_directory(&root)?;
        #[cfg(unix)]
        try_exclusive(&directory)?;
        verify_directory(&directory, &root)?;
        let control = open_control(&root)?;
        try_exclusive(&control)?;
        verify_directory(&directory, &root)?;
        #[cfg(unix)]
        for ancestor in &ancestors {
            ancestor.verify()?;
        }
        let claim = Arc::new(Claim {
            root,
            directory,
            _control: control,
            private: false,
            #[cfg(unix)]
            ancestors,
        });
        registry.push(Arc::downgrade(&claim));
        Ok(Self {
            owner: owner.clone(),
            claim,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadPoint {
    BeforeOpen,
    AfterOpen,
    AfterRead,
}

fn read_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The managed file changed during its bounded read",
    )
}

fn validate_filename(filename: &Path) -> io::Result<()> {
    let mut components = filename.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) if filename.as_os_str() == name => {
            validate_leaf(name)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Use one managed file name without traversal",
        )),
    }
}

fn validate_leaf(name: &std::ffi::OsStr) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 1024
        || name.as_encoded_bytes().contains(&0)
        || reserved_control(name)?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The managed file name is reserved or invalid",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let name = name.encode_wide().collect::<Vec<_>>();
        if name.contains(&(b':' as u16))
            || name
                .last()
                .is_some_and(|last| *last == b'.' as u16 || *last == b' ' as u16)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use an unambiguous managed file name",
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn reserved_control(name: &std::ffi::OsStr) -> io::Result<bool> {
    Ok(name == CONTROL_FILE)
}

#[cfg(windows)]
fn reserved_control(name: &std::ffi::OsStr) -> io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CompareStringOrdinal(
            left: *const u16,
            left_count: i32,
            right: *const u16,
            right_count: i32,
            ignore_case: i32,
        ) -> i32;
    }
    let left = name.encode_wide().collect::<Vec<_>>();
    let right = CONTROL_FILE.encode_utf16().collect::<Vec<_>>();
    let result = unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result == 2)
    }
}

#[derive(Eq, PartialEq)]
struct ReadStamp {
    bytes: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    changed: i64,
}

impl ReadStamp {
    fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Managed reads require a regular file",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Managed reads cannot follow reparse points",
                ));
            }
        }
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            changed: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ctime(), metadata.ctime_nsec())
            },
            #[cfg(windows)]
            changed: read_change_time(file)?,
        })
    }
}

#[cfg(unix)]
fn open_read_child(directory: &File, filename: &Path) -> io::Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(filename.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid managed file name"))?;
    let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            before.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let before = unsafe { before.assume_init() };
    if before.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Managed reads require a regular file",
        ));
    }
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if metadata.dev() != before.st_dev || metadata.ino() != before.st_ino {
            return Err(read_changed());
        }
    }
    ReadStamp::read(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn same_handles(left: &File, right: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn same_handles(left: &File, right: &File) -> io::Result<bool> {
    Ok(windows_identity(left)? == windows_identity(right)?)
}

#[cfg(any(windows, test))]
#[repr(C)]
struct NativeUnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[cfg(any(windows, test))]
#[repr(C)]
struct NativeObjectAttributes {
    length: u32,
    root: *mut std::ffi::c_void,
    name: *mut NativeUnicodeString,
    attributes: u32,
    security: *mut std::ffi::c_void,
    quality: *mut std::ffi::c_void,
}

#[cfg(any(windows, test))]
#[repr(C)]
struct NativeIoStatus {
    status: usize,
    information: usize,
}

#[cfg(windows)]
fn open_read_child(directory: &File, filename: &Path) -> io::Result<File> {
    use std::os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    };
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            handle: *mut *mut std::ffi::c_void,
            access: u32,
            attributes: *mut NativeObjectAttributes,
            status: *mut NativeIoStatus,
            allocation: *mut i64,
            file_attributes: u32,
            sharing: u32,
            disposition: u32,
            options: u32,
            extra: *mut std::ffi::c_void,
            extra_length: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }
    let mut encoded_name = filename.as_os_str().encode_wide().collect::<Vec<_>>();
    let length = u16::try_from(encoded_name.len() * 2).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "The managed file name is too long",
        )
    })?;
    let mut name = NativeUnicodeString {
        length,
        maximum_length: length,
        buffer: encoded_name.as_mut_ptr(),
    };
    let mut attributes = NativeObjectAttributes {
        length: std::mem::size_of::<NativeObjectAttributes>() as u32,
        root: directory.as_raw_handle(),
        name: &mut name,
        attributes: 0x40 | 0x1000,
        security: std::ptr::null_mut(),
        quality: std::ptr::null_mut(),
    };
    let mut status = NativeIoStatus {
        status: 0,
        information: 0,
    };
    let mut handle = std::ptr::null_mut();
    // RootDirectory is the retained directory handle; no pathname fallback can redirect this open.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            0x1 | 0x80 | 0x00100000,
            &mut attributes,
            &mut status,
            std::ptr::null_mut(),
            0,
            7,
            1,
            0x20 | 0x40 | 0x00200000,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || handle.is_null() || handle as isize == -1 {
        if !handle.is_null() && handle as isize != -1 {
            drop(unsafe { File::from_raw_handle(handle) });
        }
        return Err(if result < 0 {
            io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(result) } as i32)
        } else {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "The native managed read did not open synchronously",
            )
        });
    }
    let file = unsafe { File::from_raw_handle(handle) };
    ReadStamp::read(&file)?;
    windows_identity(&file)?;
    Ok(file)
}

#[cfg(windows)]
fn read_change_time(file: &File) -> io::Result<i64> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        created: i64,
        accessed: i64,
        modified: i64,
        changed: i64,
        attributes: u32,
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
    let mut basic = Basic::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            0,
            std::ptr::from_mut(&mut basic).cast(),
            std::mem::size_of::<Basic>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(basic.changed)
}

#[cfg(not(any(unix, windows)))]
fn open_read_child(_directory: &File, _filename: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Handle-relative managed reads are unsupported",
    ))
}
#[cfg(not(any(unix, windows)))]
fn same_handles(_left: &File, _right: &File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Managed file identities are unsupported",
    ))
}

impl Claim {
    fn verify_shallow(&self) -> io::Result<()> {
        verify_directory(&self.directory, &self.root)?;
        if self.private {
            private::verify_claim(self)?;
        }
        Ok(())
    }

    fn verify(&self) -> io::Result<()> {
        self.verify_shallow()?;
        #[cfg(unix)]
        for ancestor in &self.ancestors {
            ancestor.verify()?;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Ancestor {
    fn verify(&self) -> io::Result<()> {
        match self {
            Self::Shared(lock) => verify_directory(&lock.file, &lock.path),
            Self::Owned(claim) => claim.verify(),
        }
    }
}

#[cfg(unix)]
fn acquire_ancestors(root: &Path, claims: &[Arc<Claim>]) -> io::Result<Vec<Ancestor>> {
    let mut ancestors: Vec<Ancestor> = Vec::new();
    let mut paths: Vec<_> = root.ancestors().collect();
    paths.reverse();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            ancestors
                .last()
                .expect("directory parent is held")
                .verify()?;
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "A managed root component is not a directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            },
            Err(error) => return Err(error),
        }
        if index == paths.len() - 1 {
            break;
        }
        if let Some(claim) = claims.iter().find(|claim| claim.root == *path) {
            ancestors.push(Ancestor::Owned(claim.clone()));
            continue;
        }
        let shared = claims
            .iter()
            .flat_map(|claim| &claim.ancestors)
            .find_map(|ancestor| match ancestor {
                Ancestor::Shared(lock) if lock.path == *path => Some(lock.clone()),
                _ => None,
            });
        let lock = match shared {
            Some(lock) => lock,
            None => {
                let file = open_directory(path)?;
                file.try_lock_shared().map_err(lock_error)?;
                verify_directory(&file, path)?;
                Arc::new(DirectoryLock {
                    path: path.to_path_buf(),
                    file,
                })
            }
        };
        ancestors.push(Ancestor::Shared(lock));
    }
    Ok(ancestors)
}

pub fn resolve_root(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Managed directories must be explicit",
        ));
    }
    let absolute = std::path::absolute(path)?;
    let mut existing = absolute.clone();
    let mut suffix = Vec::new();
    let mut canonical = loop {
        match fs::canonicalize(&existing) {
            Ok(path) => break path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let component = existing.components().next_back().ok_or(error)?;
                suffix.push(component.as_os_str().to_os_string());
                if !existing.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "No existing ancestor for managed root",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    };
    for component in suffix.into_iter().rev() {
        if component == ".." {
            canonical.pop();
        } else if component != "." {
            canonical.push(component);
        }
        match fs::canonicalize(&canonical) {
            Ok(path) => canonical = path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(canonical)
}

fn open_directory(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x02000000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A managed root must be a directory",
        ));
    }
    Ok(file)
}

fn open_control(root: &Path) -> io::Result<File> {
    let path = root.join(CONTROL_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The directory control file must be a regular file",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "The directory control file cannot have hard links",
                    ));
                }
            }
            open_existing_control(&path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(root)?.permissions().mode() & 0o222 == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "A read-only root needs an existing directory control file",
                    ));
                }
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(3);
            }
            match options.open(&path) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_control(root),
                result => result,
            }
        }
        Err(error) => Err(error),
    }
}

fn open_existing_control(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3);
    }
    let file = options.open(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || !equivalent(&file, path)? {
        return Err(io::Error::other(
            "The directory control file changed while opening",
        ));
    }
    Ok(file)
}

fn try_exclusive(file: &File) -> io::Result<()> {
    file.try_lock().map_err(lock_error)
}

fn lock_error(error: std::fs::TryLockError) -> io::Error {
    match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "The managed directory overlaps another owner's live claim",
        ),
        std::fs::TryLockError::Error(error) => error,
    }
}

fn verify_directory(file: &File, path: &Path) -> io::Result<()> {
    if !equivalent(file, path)? || fs::canonicalize(path)? != path {
        return Err(io::Error::other("The managed directory identity changed"));
    }
    Ok(())
}

pub(crate) fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    equivalent(file, path)
}

#[cfg(unix)]
fn equivalent(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let held = file.metadata()?;
    let current = fs::metadata(path)?;
    Ok(held.dev() == current.dev() && held.ino() == current.ino())
}

#[cfg(any(windows, test))]
#[repr(C)]
#[derive(Debug, Default, Eq, PartialEq)]
struct WindowsFileIdentity {
    volume: u64,
    file_id: [u8; 16],
}

#[cfg(windows)]
fn windows_identity(file: &File) -> io::Result<WindowsFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: *mut std::ffi::c_void,
            class: i32,
            information: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    const FILE_ID_INFO: i32 = 18;
    let mut result = WindowsFileIdentity::default();
    // FileIdInfo retains all 128 ID bits; the legacy 64-bit index is not unique on ReFS.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FILE_ID_INFO,
            std::ptr::from_mut(&mut result).cast(),
            std::mem::size_of::<WindowsFileIdentity>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(result)
}

#[cfg(windows)]
fn equivalent(file: &File, path: &Path) -> io::Result<bool> {
    let mut options = OpenOptions::new();
    options.read(true);
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x02000000).share_mode(3);
    }
    let current = options.open(path)?;
    Ok(windows_identity(file)? == windows_identity(&current)?)
}

#[cfg(not(any(unix, windows)))]
fn equivalent(_file: &File, _path: &Path) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Directory identities are unsupported on this platform",
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteStage {
    Validate,
    Stage,
    Serialize,
    SyncFile,
    Rename,
    SyncDirectory,
}

#[derive(Debug)]
pub struct AtomicWriteError {
    pub stage: WriteStage,
    committed: bool,
    source: io::Error,
}

impl AtomicWriteError {
    #[cfg(test)]
    pub(crate) fn test_committed(stage: WriteStage, source: io::Error) -> Self {
        Self::new(stage, true, source)
    }

    pub(crate) fn validation(source: io::Error) -> Self {
        Self::new(WriteStage::Validate, false, source)
    }

    fn new(stage: WriteStage, committed: bool, source: io::Error) -> Self {
        Self {
            stage,
            committed,
            source,
        }
    }

    pub fn committed(&self) -> bool {
        self.committed
    }

    pub fn root_unavailable(&self) -> bool {
        is_root_unavailable(&self.source)
    }

    pub fn into_io_error(self) -> io::Error {
        self.source
    }
}

impl std::fmt::Display for AtomicWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Atomic JSON write failed at {:?}; committed={}: {}",
            self.stage, self.committed, self.source
        )
    }
}

impl std::error::Error for AtomicWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomicCommit {
    pub directory_synced: bool,
}

#[derive(Clone, Copy)]
enum WriteAuthority<'a> {
    Unmanaged,
    Lease(&'a DirectoryLease),
    Scope(&'a LiveRoots),
}

impl WriteAuthority<'_> {
    fn failure(self, stage: WriteStage, committed: bool, source: io::Error) -> AtomicWriteError {
        AtomicWriteError::new(stage, committed, self.verify().err().unwrap_or(source))
    }

    fn verify(self) -> io::Result<()> {
        match self {
            Self::Unmanaged => Ok(()),
            Self::Lease(lease) => lease.verify(),
            Self::Scope(scope) => scope.verify().map_err(Into::into),
        }
    }
}

struct StagedWrite<'a> {
    file: Option<tempfile::NamedTempFile>,
    authority: WriteAuthority<'a>,
}

impl Drop for StagedWrite<'_> {
    fn drop(&mut self) {
        if let Some(file) = &self.file
            && self.authority.verify().is_ok()
            && fs::symlink_metadata(file.path())
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            && same_file(file.as_file(), file.path()).unwrap_or(false)
        {
            let _ = fs::remove_file(file.path());
        }
    }
}

struct GuardedWriter<'a, W> {
    writer: W,
    authority: WriteAuthority<'a>,
}

impl<W: Write> Write for GuardedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.authority.verify()?;
        self.writer.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.authority.verify()?;
        self.writer.flush()
    }
}

pub fn atomic_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> Result<AtomicCommit, AtomicWriteError> {
    atomic_json_checked(path, value, WriteAuthority::Unmanaged)
}

fn atomic_json_checked<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    authority: WriteAuthority<'_>,
) -> Result<AtomicCommit, AtomicWriteError> {
    atomic_json_observed(path, value, authority, |_, _| {})
}

fn atomic_json_observed<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    authority: WriteAuthority<'_>,
    mut checkpoint: impl FnMut(WriteStage, bool),
) -> Result<AtomicCommit, AtomicWriteError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    authority
        .verify()
        .map_err(|error| authority.failure(WriteStage::Stage, false, error))?;
    // Cleanup is explicit because a moved root must not redirect a temporary-path unlink.
    let file = tempfile::Builder::new()
        .disable_cleanup(true)
        .tempfile_in(parent)
        .map_err(|error| authority.failure(WriteStage::Stage, false, error))?;
    let mut staged = StagedWrite {
        file: Some(file),
        authority,
    };
    {
        let file = staged.file.as_mut().expect("staged file is retained");
        let guarded = GuardedWriter {
            writer: file.as_file_mut(),
            authority,
        };
        let mut writer = BufWriter::new(guarded);
        serde_json::to_writer(&mut writer, value)
            .map_err(|error| authority.failure(WriteStage::Serialize, false, error.into()))?;
        writer
            .write_all(b"\n")
            .and_then(|()| writer.flush())
            .map_err(|error| authority.failure(WriteStage::Serialize, false, error))?;
    }
    authority
        .verify()
        .map_err(|error| authority.failure(WriteStage::SyncFile, false, error))?;
    staged
        .file
        .as_ref()
        .expect("staged file is retained")
        .as_file()
        .sync_all()
        .map_err(|error| authority.failure(WriteStage::SyncFile, false, error))?;
    authority
        .verify()
        .map_err(|error| authority.failure(WriteStage::Rename, false, error))?;
    let file = staged.file.take().expect("staged file is retained");
    if let Err(error) = file.persist(path) {
        staged.file = Some(error.file);
        return Err(authority.failure(WriteStage::Rename, false, error.error));
    }
    checkpoint(WriteStage::Rename, true);
    #[cfg(unix)]
    {
        authority
            .verify()
            .map_err(|error| authority.failure(WriteStage::SyncDirectory, true, error))?;
        open_directory(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| authority.failure(WriteStage::SyncDirectory, true, error))?;
    }
    Ok(AtomicCommit {
        directory_synced: cfg!(unix),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{BufRead, Read},
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    const CHILD_ROOT: &str = "JUICED_STORAGE_TEST_ROOT";

    #[cfg(unix)]
    struct DuringSerialize<F>(F);

    #[cfg(unix)]
    impl<F: Fn()> Serialize for DuringSerialize<F> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            (self.0)();
            42u64.serialize(serializer)
        }
    }

    #[test]
    fn generated_ids_have_the_existing_opaque_namespace_without_filesystem_state() {
        let ids: std::collections::HashSet<_> = (0..32).map(|_| generated_id().unwrap()).collect();
        assert_eq!(ids.len(), 32);
        assert!(ids.iter().all(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }));
    }

    #[test]
    fn scopes_are_bounded_retain_claims_and_reject_foreign_authority() {
        let temp = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(temp.path().join("owned")).unwrap();
        let other = DirectoryLease::acquire(temp.path().join("other")).unwrap();
        assert_eq!(
            LiveRoots::new(Vec::new()).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            LiveRoots::new(vec![lease.clone(); 65])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let scope = LiveRoots::new(vec![lease.clone(), lease.clone()]).unwrap();
        assert_eq!(scope.roots.len(), 1);
        assert!(scope.contains(&lease));
        assert!(scope.contains(&lease.claim(lease.root()).unwrap()));
        assert!(!scope.contains(&other));
        let error = scope
            .write_json(&other, "untouched.json", &json!(1))
            .unwrap_err();
        assert!(!error.committed());
        assert!(!error.root_unavailable());
        assert_eq!(error.into_io_error().kind(), io::ErrorKind::InvalidInput);
        assert!(!other.root().join("untouched.json").exists());
        let root = lease.root().to_owned();
        let retained = Arc::downgrade(&lease.claim);
        drop(lease);
        assert!(retained.upgrade().is_some());
        assert!(DirectoryLease::acquire(&root).is_err());
        drop(scope);
        assert!(retained.upgrade().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn shared_scope_observes_each_retained_identity_once_across_owned_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let config = DirectoryLease::acquire(temp.path().join("config")).unwrap();
        let artifacts = config.claim(config.root().join("artifacts")).unwrap();
        let cache = config.claim(config.root().join("cache")).unwrap();
        let scope = LiveRoots::new(vec![
            config.clone(),
            artifacts.clone(),
            cache.clone(),
            cache.clone(),
        ])
        .unwrap();
        assert_eq!(scope.roots.len(), 3);
        assert_eq!(scope.observations.len(), config.claim.ancestors.len() + 3);
        for lease in [&config, &artifacts, &cache] {
            assert_eq!(scope.observations.iter().filter(|observation| {
                matches!(observation, Observation::Claim(claim) if Arc::ptr_eq(claim, &lease.claim))
            }).count(), 1);
        }
        for ancestor in &config.claim.ancestors {
            let Ancestor::Shared(expected) = ancestor else {
                panic!("an initial claim retains shared ancestors");
            };
            assert_eq!(scope.observations.iter().filter(|observation| {
                matches!(observation, Observation::Shared(lock) if Arc::ptr_eq(lock, expected))
            }).count(), 1);
        }
        scope.verify().unwrap();
        let nested = artifacts.claim(artifacts.root().join("nested")).unwrap();
        let nested_scope = LiveRoots::single(nested).unwrap();
        assert_eq!(
            nested_scope.observations.len(),
            config.claim.ancestors.len() + 3
        );
        assert!(!nested_scope.observations.iter().any(|observation| {
            matches!(observation, Observation::Claim(claim) if Arc::ptr_eq(claim, &cache.claim))
        }));
        nested_scope.verify().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn equal_ancestor_paths_do_not_merge_independently_retained_locks() {
        let temp = tempfile::tempdir().unwrap();
        let first = DirectoryLease::acquire(temp.path().join("first")).unwrap();
        let second = DirectoryLease::acquire(temp.path().join("second")).unwrap();
        let scope = LiveRoots::new(vec![first.clone(), second.clone()]).unwrap();
        assert_eq!(first.claim.ancestors.len(), second.claim.ancestors.len());
        for (left, right) in first.claim.ancestors.iter().zip(&second.claim.ancestors) {
            let (Ancestor::Shared(left), Ancestor::Shared(right)) = (left, right) else {
                panic!("independent owners retain their own ancestors");
            };
            assert_eq!(left.path, right.path);
            assert!(!Arc::ptr_eq(left, right));
        }
        assert_eq!(
            scope.observations.len(),
            first.claim.ancestors.len() + second.claim.ancestors.len() + 2
        );
        scope.verify().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn observed_owned_ancestors_do_not_gain_write_membership_and_remain_held_until_scope_drop() {
        let temp = tempfile::tempdir().unwrap();
        let parent = DirectoryLease::acquire(temp.path().join("parent")).unwrap();
        let child = parent.claim(parent.root().join("child")).unwrap();
        let scope = LiveRoots::single(child.clone()).unwrap();
        assert!(scope.observations.iter().any(|observation| {
            matches!(observation, Observation::Claim(claim) if Arc::ptr_eq(claim, &parent.claim))
        }));
        assert!(!scope.contains(&parent));
        let count = fs::read_dir(parent.root()).unwrap().count();
        let error = scope
            .write_json(&parent, "unapproved.json", &json!(1))
            .unwrap_err();
        assert_eq!(error.into_io_error().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_dir(parent.root()).unwrap().count(), count);
        scope
            .write_json(&child, "approved.json", &json!(2))
            .unwrap();
        let path = parent.root().to_owned();
        let retained = Arc::downgrade(&parent.claim);
        drop(child);
        drop(parent);
        scope.verify().unwrap();
        assert!(retained.upgrade().is_some());
        assert!(DirectoryLease::acquire(&path).is_err());
        drop(scope);
        assert!(retained.upgrade().is_none());
        let _released = acquire_after_release(&path);
    }

    #[cfg(unix)]
    #[test]
    fn replaced_shared_ancestor_is_detected_even_when_the_root_directory_inode_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let ancestor = temp.path().join("ancestor");
        let root = DirectoryLease::acquire(ancestor.join("store")).unwrap();
        let scope = LiveRoots::single(root.clone()).unwrap();
        scope.verify().unwrap();
        let retired = temp.path().join("retired");
        fs::rename(&ancestor, &retired).unwrap();
        fs::create_dir(&ancestor).unwrap();
        fs::write(ancestor.join("foreign"), b"preserved foreign bytes").unwrap();
        fs::rename(retired.join("store"), ancestor.join("store")).unwrap();
        verify_directory(&root.claim.directory, root.root()).unwrap();
        let error: io::Error = scope.verify().unwrap_err().into();
        assert!(is_root_unavailable(&error));
        assert!(
            scope
                .write_json(&root, "blocked.json", &json!(1))
                .unwrap_err()
                .root_unavailable()
        );
        assert!(!root.root().join("blocked.json").exists());
        fs::rename(ancestor.join("store"), retired.join("store")).unwrap();
        fs::rename(&ancestor, temp.path().join("foreign-retained")).unwrap();
        fs::rename(&retired, &ancestor).unwrap();
        scope.verify().unwrap();
        scope.write_json(&root, "allowed.json", &json!(2)).unwrap();
        assert_eq!(
            fs::read(temp.path().join("foreign-retained/foreign")).unwrap(),
            b"preserved foreign bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn alias_retarget_keeps_scope_identity_and_each_later_verification_observes_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        let foreign = temp.path().join("foreign");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("canary"), b"untouched").unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let root = DirectoryLease::acquire(&alias).unwrap();
        let scope = LiveRoots::single(root.clone()).unwrap();
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&foreign, &alias).unwrap();
        scope.verify().unwrap();
        scope.write_json(&root, "owned.json", &json!(1)).unwrap();
        assert!(!foreign.join("owned.json").exists());
        fs::rename(&original, temp.path().join("retired")).unwrap();
        fs::create_dir(&original).unwrap();
        let error: io::Error = scope.verify().unwrap_err().into();
        assert!(is_root_unavailable(&error));
        assert_eq!(fs::read(foreign.join("canary")).unwrap(), b"untouched");
    }

    #[test]
    fn scope_verification_does_not_lock_the_claim_registry() {
        let temp = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(temp.path()).unwrap();
        let scope = LiveRoots::single(lease.clone()).unwrap();
        let registry = lease.owner.claims.lock().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || sender.send(scope.verify().is_ok()).unwrap());
        let result = receiver.recv_timeout(Duration::from_secs(1));
        drop(registry);
        thread.join().unwrap();
        assert!(result.unwrap());
    }

    #[test]
    fn only_typed_root_failures_are_classified_as_root_unavailable() {
        assert!(!is_root_unavailable(&io::Error::other(
            "A managed storage root is unavailable"
        )));
        assert!(!is_root_unavailable(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        let typed: io::Error = RootUnavailable {
            source: io::Error::from(io::ErrorKind::NotFound),
        }
        .into();
        assert!(is_root_unavailable(&typed));
        assert_eq!(typed.to_string(), "A managed storage root is unavailable");
        let wrapped = AtomicWriteError::new(WriteStage::Rename, true, typed);
        assert!(wrapped.root_unavailable());
        assert!(wrapped.committed());
    }

    #[cfg(unix)]
    #[test]
    fn losing_each_scoped_root_blocks_other_roots_but_not_independent_scopes() {
        for replaced in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let roots = DirectoryLease::acquire_many(
                ["config", "cache", "artifacts"].map(|name| temp.path().join(name)),
            )
            .unwrap();
            let scope = LiveRoots::new(roots.clone()).unwrap();
            let independent = DirectoryLease::acquire(temp.path().join("independent")).unwrap();
            let independent_scope = LiveRoots::single(independent.clone()).unwrap();
            let moved = temp.path().join("retired");
            fs::rename(roots[replaced].root(), &moved).unwrap();
            fs::create_dir(roots[replaced].root()).unwrap();
            fs::write(roots[replaced].root().join("foreign"), b"unchanged").unwrap();
            let error: io::Error = scope.verify().unwrap_err().into();
            assert!(is_root_unavailable(&error));
            assert!(is_root_unavailable(&roots[replaced].verify().unwrap_err()));
            assert!(
                LiveRoots::new(roots.clone())
                    .err()
                    .is_some_and(|error| is_root_unavailable(&error))
            );
            for root in &roots {
                let error = scope
                    .write_json(root, "blocked.json", &json!(1))
                    .unwrap_err();
                assert!(error.root_unavailable());
                assert!(!error.committed());
                assert!(!root.root().join("blocked.json").exists());
            }
            independent_scope
                .write_json(&independent, "allowed.json", &json!(2))
                .unwrap();
            assert_eq!(
                fs::read(roots[replaced].root().join("foreign")).unwrap(),
                b"unchanged"
            );
            assert_eq!(fs::read_dir(roots[replaced].root()).unwrap().count(), 1);
            fs::remove_dir_all(roots[replaced].root()).unwrap();
            fs::rename(&moved, roots[replaced].root()).unwrap();
            scope.verify().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn scope_failure_during_serialization_preserves_staging_and_original_generation() {
        let temp = tempfile::tempdir().unwrap();
        let destination = DirectoryLease::acquire(temp.path().join("destination")).unwrap();
        let second = destination.claim(temp.path().join("second")).unwrap();
        let scope = LiveRoots::new(vec![destination.clone(), second.clone()]).unwrap();
        fs::write(destination.root().join("manifest.json"), b"old generation").unwrap();
        let value = DuringSerialize(|| {
            fs::rename(second.root(), temp.path().join("retired")).unwrap();
            fs::create_dir(second.root()).unwrap();
            fs::write(second.root().join("foreign"), b"untouched").unwrap();
        });
        let error = scope
            .write_json(&destination, "manifest.json", &value)
            .unwrap_err();
        assert!(error.root_unavailable());
        assert!(!error.committed());
        assert_eq!(
            fs::read(destination.root().join("manifest.json")).unwrap(),
            b"old generation"
        );
        assert_eq!(fs::read_dir(destination.root()).unwrap().count(), 3);
        assert_eq!(fs::read_dir(second.root()).unwrap().count(), 1);
        assert_eq!(
            fs::read(second.root().join("foreign")).unwrap(),
            b"untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn moved_root_cannot_redirect_staging_cleanup_to_foreign_file() {
        let temp = tempfile::tempdir().unwrap();
        let destination = DirectoryLease::acquire(temp.path().join("destination")).unwrap();
        let scope = LiveRoots::single(destination.clone()).unwrap();
        let retired = temp.path().join("retired");
        let foreign_name = std::cell::RefCell::new(None);
        let value = DuringSerialize(|| {
            let staged = fs::read_dir(destination.root())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .find(|name| name != CONTROL_FILE)
                .unwrap();
            fs::rename(destination.root(), &retired).unwrap();
            fs::create_dir(destination.root()).unwrap();
            fs::write(destination.root().join(&staged), b"foreign data").unwrap();
            *foreign_name.borrow_mut() = Some(staged);
        });
        let error = scope
            .write_json(&destination, "manifest.json", &value)
            .unwrap_err();
        assert!(error.root_unavailable());
        assert!(!error.committed());
        let name = foreign_name.into_inner().unwrap();
        assert_eq!(
            fs::read(destination.root().join(&name)).unwrap(),
            b"foreign data"
        );
        assert!(retired.join(&name).exists());
        assert_eq!(fs::read_dir(destination.root()).unwrap().count(), 1);
        assert!(!destination.root().join("manifest.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn serialization_io_wrapping_preserves_typed_root_refusal() {
        let temp = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(temp.path().join("owned")).unwrap();
        let scope = LiveRoots::single(lease.clone()).unwrap();
        fs::rename(lease.root(), temp.path().join("retired")).unwrap();
        let mut bytes = Vec::new();
        let mut writer = GuardedWriter {
            writer: &mut bytes,
            authority: WriteAuthority::Scope(&scope),
        };
        let error: io::Error = serde_json::to_writer(&mut writer, &json!(42))
            .unwrap_err()
            .into();
        assert!(is_root_unavailable(&error));
        assert!(bytes.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn root_failure_after_rename_preserves_committed_outcome() {
        let temp = tempfile::tempdir().unwrap();
        let destination = DirectoryLease::acquire(temp.path().join("destination")).unwrap();
        let scope = LiveRoots::single(destination.clone()).unwrap();
        let path = destination.root().join("manifest.json");
        let retired = temp.path().join("retired");
        let error = atomic_json_observed(
            &path,
            &json!({"saved": true}),
            WriteAuthority::Scope(&scope),
            |stage, committed| {
                assert_eq!(stage, WriteStage::Rename);
                assert!(committed);
                fs::rename(destination.root(), &retired).unwrap();
                fs::create_dir(destination.root()).unwrap();
                fs::write(&path, b"foreign generation").unwrap();
            },
        )
        .unwrap_err();
        assert!(error.committed());
        assert!(error.root_unavailable());
        assert_eq!(error.stage, WriteStage::SyncDirectory);
        assert_eq!(fs::read(&path).unwrap(), b"foreign generation");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(retired.join("manifest.json")).unwrap()
            )
            .unwrap(),
            json!({"saved": true})
        );
        assert_eq!(fs::read_dir(destination.root()).unwrap().count(), 1);
    }

    #[test]
    fn native_relative_open_structures_have_the_documented_c_layout() {
        let pointer = std::mem::size_of::<usize>();
        let mut name = NativeUnicodeString {
            length: 2,
            maximum_length: 2,
            buffer: std::ptr::null_mut(),
        };
        let attributes = NativeObjectAttributes {
            length: std::mem::size_of::<NativeObjectAttributes>() as u32,
            root: std::ptr::null_mut(),
            name: &mut name,
            attributes: 0x1040,
            security: std::ptr::null_mut(),
            quality: std::ptr::null_mut(),
        };
        let status = NativeIoStatus {
            status: 0,
            information: 1,
        };
        assert_eq!((name.length, name.maximum_length), (2, 2));
        assert!(name.buffer.is_null());
        assert_eq!(
            attributes.length as usize,
            std::mem::size_of::<NativeObjectAttributes>()
        );
        assert!(
            attributes.root.is_null()
                && !attributes.name.is_null()
                && attributes.security.is_null()
                && attributes.quality.is_null()
        );
        assert_eq!(attributes.attributes, 0x1040);
        assert_eq!((status.status, status.information), (0, 1));
        assert_eq!(
            std::mem::size_of::<NativeUnicodeString>(),
            if pointer == 8 { 16 } else { 8 }
        );
        assert_eq!(std::mem::offset_of!(NativeUnicodeString, buffer), pointer);
        assert_eq!(
            std::mem::size_of::<NativeObjectAttributes>(),
            if pointer == 8 { 48 } else { 24 }
        );
        assert_eq!(std::mem::offset_of!(NativeObjectAttributes, root), pointer);
        assert_eq!(
            std::mem::offset_of!(NativeObjectAttributes, name),
            2 * pointer
        );
        assert_eq!(
            std::mem::offset_of!(NativeObjectAttributes, attributes),
            3 * pointer
        );
        assert_eq!(
            std::mem::offset_of!(NativeObjectAttributes, security),
            4 * pointer
        );
        assert_eq!(
            std::mem::offset_of!(NativeObjectAttributes, quality),
            5 * pointer
        );
        assert_eq!(std::mem::size_of::<NativeIoStatus>(), 2 * pointer);
        assert_eq!(std::mem::offset_of!(NativeIoStatus, information), pointer);
    }

    #[test]
    fn managed_reads_are_bounded_and_preserve_exact_bytes_without_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = DirectoryLease::acquire(temporary.path().join("store")).unwrap();
        let bytes = b"{ \"spaced\" : true }\n\n";
        fs::write(root.root().join("config.json"), bytes).unwrap();
        assert_eq!(
            root.file_path("config.json").unwrap(),
            root.root().join("config.json")
        );
        assert_eq!(
            root.read_bytes("config.json", bytes.len() as u64).unwrap(),
            bytes
        );
        assert_eq!(fs::read(root.root().join("config.json")).unwrap(), bytes);
        assert_eq!(fs::read_dir(root.root()).unwrap().count(), 2);
        let mut reached_read = false;
        let too_small = root.read_bytes_checked(
            Path::new("config.json"),
            bytes.len() as u64 - 1,
            |point, _| {
                if point == ReadPoint::AfterOpen {
                    reached_read = true;
                }
            },
        );
        assert_eq!(too_small.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(!reached_read);
        for maximum in [0, MAXIMUM_READ_BYTES + 1] {
            assert_eq!(
                root.read_bytes("config.json", maximum).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        fs::write(root.root().join("empty"), b"").unwrap();
        assert!(root.read_bytes("empty", 1).unwrap().is_empty());
    }

    #[test]
    fn filename_validation_blocks_traversal_and_control_file_read_or_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let root = DirectoryLease::acquire(temporary.path().join("store")).unwrap();
        let marker = fs::read(root.root().join(CONTROL_FILE)).unwrap();
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "child/config.json",
            "./config.json",
            "config.json/",
            CONTROL_FILE,
        ] {
            assert_eq!(
                root.file_path(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                root.read_bytes(name, 128).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let error = root
            .write_json(CONTROL_FILE, &json!({"replace":true}))
            .unwrap_err();
        assert_eq!(error.stage, WriteStage::Validate);
        assert!(!error.committed());
        assert_eq!(fs::read(root.root().join(CONTROL_FILE)).unwrap(), marker);
        assert_eq!(fs::read_dir(root.root()).unwrap().count(), 1);
    }

    #[test]
    fn managed_read_rejects_missing_files_and_directory_leaves_without_creation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = DirectoryLease::acquire(temporary.path().join("store")).unwrap();
        assert_eq!(
            root.read_bytes("missing", 1024).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        fs::create_dir(root.root().join("directory")).unwrap();
        assert!(root.read_bytes("directory", 1024).is_err());
        assert_eq!(fs::read_dir(root.root()).unwrap().count(), 2);
    }

    #[test]
    fn replaced_leaf_or_content_change_during_read_cannot_return_a_successful_binding() {
        let temporary = tempfile::tempdir().unwrap();
        let root = DirectoryLease::acquire(temporary.path().join("store")).unwrap();
        let path = root.root().join("config.json");
        fs::write(&path, b"original").unwrap();
        let replaced = root.read_bytes_checked(Path::new("config.json"), 128, |point, _| {
            if point == ReadPoint::AfterOpen {
                fs::rename(&path, root.root().join("retired")).unwrap();
                fs::write(&path, b"modified").unwrap();
            }
        });
        assert_eq!(replaced.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), b"modified");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let changed = root.read_bytes_checked(Path::new("config.json"), 128, |point, _| {
            if point == ReadPoint::AfterRead {
                fs::write(&path, b"same-len").unwrap();
                File::open(&path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
        });
        assert_eq!(changed.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), b"same-len");
    }

    #[cfg(unix)]
    #[test]
    fn alias_retarget_does_not_redirect_managed_reads_or_returned_canonical_paths() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let original = temporary.path().join("original");
        let foreign = temporary.path().join("foreign");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&foreign).unwrap();
        fs::write(original.join("config.json"), b"original").unwrap();
        fs::write(foreign.join("config.json"), b"foreign").unwrap();
        let alias = temporary.path().join("alias");
        symlink(&original, &alias).unwrap();
        let root = DirectoryLease::acquire(&alias).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(&foreign, &alias).unwrap();
        assert_eq!(root.read_bytes("config.json", 128).unwrap(), b"original");
        assert_eq!(
            root.file_path("config.json").unwrap(),
            fs::canonicalize(&original).unwrap().join("config.json")
        );
        assert_eq!(fs::read(foreign.join("config.json")).unwrap(), b"foreign");
    }

    #[cfg(unix)]
    #[test]
    fn root_replacement_between_validation_and_open_keeps_the_original_directory_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("store");
        let root = DirectoryLease::acquire(&path).unwrap();
        fs::write(path.join("config.json"), b"original").unwrap();
        let original = File::open(path.join("config.json")).unwrap();
        let result = root.read_bytes_checked(Path::new("config.json"), 128, |point, file| {
            if point == ReadPoint::BeforeOpen {
                fs::rename(&path, temporary.path().join("retired")).unwrap();
                fs::create_dir(&path).unwrap();
                fs::write(path.join("config.json"), b"foreign").unwrap();
            } else if point == ReadPoint::AfterOpen {
                assert!(same_handles(&original, file).unwrap());
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(path.join("config.json")).unwrap(), b"foreign");
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
        assert_eq!(
            fs::read(temporary.path().join("retired/config.json")).unwrap(),
            b"original"
        );
    }

    #[cfg(unix)]
    #[test]
    fn leaf_symlinks_and_fifos_are_rejected_without_following_or_blocking() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let temporary = tempfile::tempdir().unwrap();
        let root = DirectoryLease::acquire(temporary.path().join("store")).unwrap();
        let external = temporary.path().join("external");
        fs::write(&external, b"external-private-config").unwrap();
        symlink(&external, root.root().join("linked")).unwrap();
        assert!(root.read_bytes("linked", 128).is_err());
        let fifo = std::ffi::CString::new(root.root().join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let retained = root.clone();
        std::thread::spawn(move || {
            let _ = send.send(retained.read_bytes("fifo", 128));
        });
        assert!(
            receive
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
        assert_eq!(fs::read(external).unwrap(), b"external-private-config");
    }

    #[cfg(windows)]
    #[test]
    fn windows_control_aliases_and_alternate_stream_names_are_not_managed_filenames() {
        let temporary = tempfile::tempdir().unwrap();
        let root = DirectoryLease::acquire(temporary.path().join("store")).unwrap();
        for filename in [
            ".JUICED-STORE.LOCK",
            ".juiced-store.lock.",
            "config.json ",
            "config.json:stream",
        ] {
            assert_eq!(
                root.file_path(filename).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(root.write_json(filename, &json!({"replace":true})).is_err());
        }
    }

    #[test]
    fn windows_identity_compares_volume_and_all_128_file_id_bits() {
        assert_eq!(std::mem::size_of::<WindowsFileIdentity>(), 24);
        assert_eq!(std::mem::offset_of!(WindowsFileIdentity, file_id), 8);
        let original = WindowsFileIdentity {
            volume: 7,
            file_id: [1; 16],
        };
        let mut different_upper_bits = WindowsFileIdentity {
            volume: original.volume,
            file_id: original.file_id,
        };
        different_upper_bits.file_id[15] ^= 1;
        assert_ne!(original, different_upper_bits);
        assert_ne!(
            original,
            WindowsFileIdentity {
                volume: original.volume + (1 << 32),
                file_id: original.file_id,
            }
        );
        assert_eq!(
            original,
            WindowsFileIdentity {
                volume: original.volume,
                file_id: original.file_id,
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_retained_file_identity_rejects_replacement() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("artifact");
        fs::write(&path, b"original").unwrap();
        let retained = OpenOptions::new()
            .read(true)
            .share_mode(7)
            .open(&path)
            .unwrap();
        assert!(same_file(&retained, &path).unwrap());
        fs::rename(&path, fixture.path().join("retired")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(!same_file(&retained, &path).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_claim_checks_exact_directory_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let lease = DirectoryLease::acquire(&path).unwrap();
        let alias = lease.claim(path.join(".")).unwrap();
        let sibling = fixture.path().join("sibling");
        fs::create_dir(&sibling).unwrap();
        assert_eq!(lease.root(), alias.root());
        assert!(same_file(&lease.claim.directory, lease.root()).unwrap());
        assert!(!same_file(&lease.claim.directory, &sibling).unwrap());
        lease.verify().unwrap();
        alias.verify().unwrap();
    }

    fn child_command(path: &Path, expected: bool, hold: bool) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        let module = module_path!().split_once("::").unwrap().1;
        command
            .args([
                "--exact",
                &format!("{module}::lease_subprocess"),
                "--nocapture",
            ])
            .env(CHILD_ROOT, path)
            .env(
                "JUICED_STORAGE_TEST_EXPECTED",
                if expected { "yes" } else { "no" },
            )
            .env("JUICED_STORAGE_TEST_HOLD", if hold { "yes" } else { "no" });
        command
    }

    fn probe(path: &Path, expected: bool) {
        let output = child_command(path, expected, false).output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn acquire_after_release(path: &Path) -> DirectoryLease {
        super::after_test_release(path, || DirectoryLease::acquire(path))
    }

    #[test]
    fn lease_subprocess() {
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            return;
        };
        let expected = std::env::var("JUICED_STORAGE_TEST_EXPECTED").unwrap() == "yes";
        let path = PathBuf::from(root);
        let result = if expected {
            Ok(acquire_after_release(&path))
        } else {
            DirectoryLease::acquire(&path)
        };
        assert_eq!(
            result.is_ok(),
            std::env::var("JUICED_STORAGE_TEST_EXPECTED").unwrap() == "yes"
        );
        if std::env::var("JUICED_STORAGE_TEST_HOLD").unwrap() == "yes" {
            assert!(result.is_ok());
            println!("JUICED_STORAGE_READY");
            io::stdout().flush().unwrap();
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input).unwrap();
        }
    }

    struct HeldChild(Child);

    impl Drop for HeldChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn conflicting_processes_reject_and_release_allows_retry() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let lease = DirectoryLease::acquire(&path).unwrap();
        probe(&path, false);
        drop(lease);
        probe(&path, true);
    }

    #[test]
    fn aliases_share_one_owner_without_releasing_the_live_claim() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let lease = DirectoryLease::acquire(&path).unwrap();
        let alias = lease.claim(path.join(".")).unwrap();
        assert_eq!(lease.root(), alias.root());
        drop(lease);
        probe(&path, false);
        drop(alias);
        probe(&path, true);
    }

    #[cfg(unix)]
    #[test]
    fn parent_claim_blocks_child_before_missing_descendants_are_created() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let lease = DirectoryLease::acquire(&path).unwrap();
        let child = path.join("absent/child");
        probe(&child, false);
        assert!(!path.join("absent").exists());
        drop(lease);
        probe(&child, true);
    }

    #[cfg(unix)]
    #[test]
    fn child_claim_blocks_parent_without_publishing_an_ancestor_control_file() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("store");
        let child = parent.join("child");
        let lease = DirectoryLease::acquire(&child).unwrap();
        probe(&parent, false);
        assert!(!parent.join(CONTROL_FILE).exists());
        drop(lease);
        probe(&parent, true);
    }

    #[cfg(unix)]
    #[test]
    fn nested_claim_retains_ancestor_and_rejects_broader_upgrades() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("store");
        let lease = DirectoryLease::acquire(&parent).unwrap();
        let child = lease.claim(parent.join("child")).unwrap();
        drop(lease);
        probe(&parent, false);
        child.verify().unwrap();
        assert!(child.claim(fixture.path()).is_err());
        assert!(!fixture.path().join(CONTROL_FILE).exists());
        drop(child);
        probe(&parent, true);
    }

    #[test]
    fn sibling_roots_remain_independent_and_startup_sorts_broader_roots_first() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("store");
        let claims = DirectoryLease::acquire_many([
            parent.join("left"),
            parent.clone(),
            parent.join("right"),
        ])
        .unwrap();
        assert_eq!(claims[0].root(), parent);
        drop(claims);
        let left = acquire_after_release(&parent.join("left"));
        probe(&parent.join("right"), true);
        drop(left);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_or_removing_the_control_file_does_not_release_directory_ownership() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let lease = DirectoryLease::acquire(&path).unwrap();
        fs::remove_file(path.join(CONTROL_FILE)).unwrap();
        probe(&path, false);
        fs::write(path.join(CONTROL_FILE), b"").unwrap();
        probe(&path, false);
        lease
            .write_json("state.json", &json!({"alive":true}))
            .unwrap();
        drop(lease);
        probe(&path, true);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_roots_need_existing_control_files_and_do_not_rewrite_them() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(DirectoryLease::acquire(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(path.join(CONTROL_FILE), b"retained marker").unwrap();
        fs::set_permissions(path.join(CONTROL_FILE), fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        let lease = acquire_after_release(&path);
        assert_eq!(
            fs::read(path.join(CONTROL_FILE)).unwrap(),
            b"retained marker"
        );
        drop(lease);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_identity_but_external_replacement_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let alias = fixture.path().join("alias");
        let lease = DirectoryLease::acquire(&path).unwrap();
        symlink(&path, &alias).unwrap();
        let other = lease.claim(&alias).unwrap();
        assert_eq!(other.root(), lease.root());
        probe(&alias, false);
        fs::rename(&path, fixture.path().join("moved")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(lease.verify().is_err());
        assert!(
            lease
                .write_json("state.json", &json!({"unsafe":true}))
                .is_err()
        );
        assert!(!path.join("state.json").exists());
    }

    #[test]
    fn os_process_termination_releases_the_lease() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store");
        let mut command = child_command(&path, true, true);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = HeldChild(command.spawn().unwrap());
        let stdout = child.0.stdout.take().unwrap();
        let mut reader = io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line.contains("JUICED_STORAGE_READY") {
                break;
            }
            line.clear();
        }
        probe(&path, false);
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match DirectoryLease::acquire(&path) {
                Ok(_) => break,
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("ownership survived process exit: {error}"),
            }
        }
    }

    #[test]
    fn atomic_json_replaces_complete_values_and_rejects_traversal() {
        let fixture = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(fixture.path().join("store")).unwrap();
        let value = json!({"revision":1,"values":[1,2,3]});
        let committed = lease.write_json("state.json", &value).unwrap();
        assert_eq!(committed.directory_synced, cfg!(unix));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(lease.root().join("state.json")).unwrap()
            )
            .unwrap(),
            value
        );
        lease
            .write_json("state.json", &json!({"revision":2}))
            .unwrap();
        assert_eq!(
            fs::read(lease.root().join("state.json")).unwrap(),
            b"{\"revision\":2}\n"
        );
        assert!(
            !lease
                .write_json("../outside.json", &value)
                .unwrap_err()
                .committed()
        );
        assert!(!fixture.path().join("outside.json").exists());
    }

    #[test]
    fn failed_serialization_or_rename_preserves_the_existing_generation() {
        struct Fail;
        impl Serialize for Fail {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom(
                    "controlled serialization failure",
                ))
            }
        }
        let fixture = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(fixture.path().join("store")).unwrap();
        lease
            .write_json("state.json", &json!({"revision":1}))
            .unwrap();
        let before = fs::read(lease.root().join("state.json")).unwrap();
        let error = lease.write_json("state.json", &Fail).unwrap_err();
        assert_eq!(error.stage, WriteStage::Serialize);
        assert!(!error.committed());
        assert_eq!(fs::read(lease.root().join("state.json")).unwrap(), before);
        fs::create_dir(lease.root().join("destination")).unwrap();
        let error = lease.write_json("destination", &json!({})).unwrap_err();
        assert_eq!(error.stage, WriteStage::Rename);
        assert!(!error.committed());
        assert_eq!(fs::read_dir(lease.root()).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn managed_json_parent_symlinks_cannot_escape_the_owned_root() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(fixture.path().join("store")).unwrap();
        let external = fixture.path().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, lease.root().join("link")).unwrap();
        let error = lease
            .write_json("link/state.json", &json!({"escape":true}))
            .unwrap_err();
        assert_eq!(error.stage, WriteStage::Validate);
        assert!(!error.committed());
        assert!(!external.join("state.json").exists());
    }
    #[test]
    fn source_reader_never_creates_a_directory_or_control_file() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(DirectoryReader::open_existing(temporary.path().join("missing")).is_err());
        assert!(!temporary.path().join("missing").exists());
        let bytes = b"{ \"preserved\": 123456789012345678901234567890 }\n";
        fs::write(temporary.path().join("source.json"), bytes).unwrap();
        let reader = DirectoryReader::open_existing(temporary.path()).unwrap();
        let observation = reader.observe("source.json", bytes.len() as u64).unwrap();
        assert_eq!(observation.bytes(), Some(bytes.as_slice()));
        let shared = observation.shared_bytes().unwrap();
        assert!(std::ptr::eq(
            shared.as_ptr(),
            observation.bytes().unwrap().as_ptr()
        ));
        assert!(observation.permissions().unwrap().is_some());
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
        assert!(!temporary.path().join(CONTROL_FILE).exists());
        assert_eq!(
            fs::read(temporary.path().join("source.json")).unwrap(),
            bytes
        );
        assert!(
            reader
                .observe("source.json", bytes.len() as u64 - 1)
                .is_err()
        );
        for name in ["../source.json", "nested/source.json", CONTROL_FILE] {
            assert!(reader.observe(name, 128).is_err());
        }
    }

    #[test]
    fn source_observations_detect_absence_creation_removal_and_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let reader = DirectoryReader::open_existing(temporary.path()).unwrap();
        let absent = reader.observe("optional.json", 128).unwrap();
        assert!(absent.bytes().is_none());
        assert!(absent.permissions().unwrap().is_none());
        absent.verify().unwrap();
        fs::write(temporary.path().join("optional.json"), b"original").unwrap();
        assert!(absent.verify().is_err());
        let present = reader.observe("optional.json", 128).unwrap();
        fs::write(temporary.path().join("replacement"), b"original").unwrap();
        fs::rename(
            temporary.path().join("optional.json"),
            temporary.path().join("old"),
        )
        .unwrap();
        fs::rename(
            temporary.path().join("replacement"),
            temporary.path().join("optional.json"),
        )
        .unwrap();
        assert!(present.verify().is_err());
        let current = reader.observe("optional.json", 128).unwrap();
        fs::remove_file(temporary.path().join("optional.json")).unwrap();
        assert!(current.verify().is_err());
        assert_eq!(present.bytes(), Some(b"original".as_slice()));
    }

    #[test]
    fn source_observations_detect_in_place_changes_and_preserve_the_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("source.json"), b"original").unwrap();
        let reader = DirectoryReader::open_existing(temporary.path()).unwrap();
        let observation = reader.observe("source.json", 128).unwrap();
        fs::write(temporary.path().join("source.json"), b"changed length").unwrap();
        assert!(observation.verify().is_err());
        assert_eq!(observation.bytes(), Some(b"original".as_slice()));
        assert!(!temporary.path().join(CONTROL_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn source_reader_binds_canonical_ancestors_and_refuses_symlinks_and_special_files() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("parent");
        fs::create_dir_all(parent.join("source")).unwrap();
        let alias = temporary.path().join("alias");
        symlink(parent.join("source"), &alias).unwrap();
        let reader = DirectoryReader::open_existing(&alias).unwrap();
        assert_eq!(
            reader.root(),
            fs::canonicalize(parent.join("source")).unwrap()
        );
        fs::write(parent.join("source/file"), b"bytes").unwrap();
        symlink("file", parent.join("source/link")).unwrap();
        assert!(reader.observe("link", 128).is_err());
        fs::create_dir(parent.join("source/directory")).unwrap();
        assert!(reader.observe("directory", 128).is_err());
        fs::rename(&parent, temporary.path().join("moved")).unwrap();
        fs::create_dir_all(parent.join("source")).unwrap();
        fs::write(parent.join("source/file"), b"foreign").unwrap();
        assert!(reader.verify().is_err());
        assert!(reader.observe("file", 128).is_err());
        assert_eq!(fs::read(parent.join("source/file")).unwrap(), b"foreign");
        assert!(!parent.join("source").join(CONTROL_FILE).exists());
    }
}
