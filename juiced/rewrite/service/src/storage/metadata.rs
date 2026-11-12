use super::{
    DirectoryLease, LiveRoots, ReadStamp, SourceContent, open_read_child, read_changed,
    read_child_checked, same_handles, validate_filename,
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct MetadataObservation {
    root: DirectoryLease,
    scope: Arc<LiveRoots>,
    name: PathBuf,
    file: File,
    stamp: ReadStamp,
}

impl MetadataObservation {
    pub fn sync(&self) -> io::Result<()> {
        self.verify()?;
        self.file.sync_all()?;
        self.root.claim.directory.sync_all()?;
        self.verify()
    }

    pub fn publish_new(mut self, name: &Path) -> io::Result<Self> {
        validate_filename(name)?;
        self.verify()?;
        self.file.sync_all()?;
        self.verify()?;
        rename_new(&self.root.claim.directory, &self.name, name)?;
        private_authority(&self.root, &self.scope)?;
        private_binding(&self.root, name, &self.file)?;
        self.root.claim.directory.sync_all()?;
        self.name = name.to_path_buf();
        self.stamp = ReadStamp::read(&self.file)?;
        self.verify()?;
        Ok(self)
    }

    fn digest(
        &mut self,
        buffer: &mut [u8],
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<[u8; 32]> {
        self.file.rewind()?;
        let mut observed = 0u64;
        let mut hash = Sha256::new();
        loop {
            check()?;
            self.verify()?;
            let count = self.file.read(buffer)?;
            if count == 0 {
                break;
            }
            observed = observed
                .checked_add(count as u64)
                .filter(|bytes| *bytes <= self.stamp.bytes)
                .ok_or_else(read_changed)?;
            hash.update(&buffer[..count]);
        }
        check()?;
        self.verify()?;
        if observed != self.stamp.bytes {
            return Err(read_changed());
        }
        Ok(hash.finalize().into())
    }

    pub fn bytes(&self) -> u64 {
        self.stamp.bytes
    }

    pub fn verify_digest(&mut self, expected: &str) -> io::Result<()> {
        let digest = self.digest(&mut [0u8; 65536], || Ok(()))?;
        if !matches_digest(&digest, expected) {
            return Err(read_changed());
        }
        Ok(())
    }

    pub fn verify(&self) -> io::Result<()> {
        private_authority(&self.root, &self.scope)?;
        private_binding(&self.root, &self.name, &self.file)?;
        if ReadStamp::read(&self.file)? != self.stamp {
            return Err(read_changed());
        }
        private_authority(&self.root, &self.scope)
    }
}

pub struct MetadataCopy {
    source: MetadataObservation,
    target: MetadataObservation,
    sha256: String,
}

impl MetadataCopy {
    pub fn bytes(&self) -> u64 {
        self.source.stamp.bytes
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn verify(&self) -> io::Result<()> {
        self.source.verify()?;
        self.target.verify()
    }
}

pub struct MetadataFile {
    root: DirectoryLease,
    scope: Arc<LiveRoots>,
    name: PathBuf,
    content: SourceContent,
}

impl MetadataFile {
    pub fn sync(&self) -> io::Result<()> {
        self.verify()?;
        self.content.file.sync_all()?;
        self.root.claim.directory.sync_all()?;
        self.verify()
    }

    pub fn verify_digest(&self, expected: &str) -> io::Result<()> {
        self.verify()?;
        let file = open_read_child(&self.root.claim.directory, &self.name)?;
        let stamp = ReadStamp::read(&file)?;
        if stamp != self.content.stamp {
            return Err(read_changed());
        }
        let mut observed = MetadataObservation {
            root: self.root.clone(),
            scope: self.scope.clone(),
            name: self.name.clone(),
            file,
            stamp,
        };
        let digest = observed.digest(&mut [0u8; 65536], || self.verify())?;
        if !matches_digest(&digest, expected) {
            return Err(read_changed());
        }
        Ok(())
    }

    pub fn try_lock_exclusive(&self) -> io::Result<()> {
        self.verify()?;
        self.content.file.try_lock().map_err(super::lock_error)?;
        self.verify()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.content.bytes
    }
    pub fn shared_bytes(&self) -> Arc<Vec<u8>> {
        self.content.bytes.clone()
    }

    #[cfg(unix)]
    pub(crate) fn into_observation(self) -> io::Result<MetadataObservation> {
        self.verify()?;
        let Self {
            root,
            scope,
            name,
            content,
        } = self;
        let SourceContent { bytes, file, stamp } = content;
        drop(bytes);
        let observed = MetadataObservation {
            root,
            scope,
            name,
            file,
            stamp,
        };
        observed.verify()?;
        Ok(observed)
    }

    pub fn verify(&self) -> io::Result<()> {
        self.scope.verify()?;
        private_file(&self.content.file)?;
        let current = open_read_child(&self.root.claim.directory, &self.name)?;
        private_file(&current)?;
        if !same_handles(&current, &self.content.file)?
            || ReadStamp::read(&current)? != self.content.stamp
            || ReadStamp::read(&self.content.file)? != self.content.stamp
        {
            return Err(read_changed());
        }
        self.scope.verify()?;
        Ok(())
    }
}

impl DirectoryLease {
    pub fn observe_private_metadata(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        maximum: u64,
    ) -> io::Result<MetadataObservation> {
        validate_filename(name)?;
        if maximum > super::MAXIMUM_READ_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Metadata observation exceeds its file bound",
            ));
        }
        private_authority(self, &scope)?;
        let file = open_read_child(&self.claim.directory, name)?;
        private_file(&file)?;
        let observed = MetadataObservation {
            root: self.clone(),
            scope,
            name: name.to_path_buf(),
            stamp: ReadStamp::read(&file)?,
            file,
        };
        if observed.bytes() > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Metadata observation exceeds its file bound",
            ));
        }
        observed.verify()?;
        Ok(observed)
    }

    pub fn private_entry_names(
        &self,
        scope: &LiveRoots,
        maximum: usize,
    ) -> io::Result<Vec<std::ffi::OsString>> {
        if !(1..=512).contains(&maximum) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Private namespace observations require a limit between 1 and 512 entries",
            ));
        }
        private_authority(self, scope)?;
        let names = directory_names(&self.claim.directory, maximum)?;
        private_authority(self, scope)?;
        Ok(names)
    }

    pub fn copy_new_private_metadata_to(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        target: &DirectoryLease,
        target_name: &Path,
        maximum: u64,
    ) -> io::Result<MetadataCopy> {
        self.copy_new_private_metadata_checked(scope, name, target, target_name, maximum, |_| {})
    }

    fn copy_new_private_metadata_checked(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        target: &DirectoryLease,
        target_name: &Path,
        maximum: u64,
        mut checkpoint: impl FnMut(u64),
    ) -> io::Result<MetadataCopy> {
        validate_filename(name)?;
        validate_filename(target_name)?;
        if maximum > super::MAXIMUM_READ_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Metadata copy exceeds its file bound",
            ));
        }
        private_authority(self, &scope)?;
        private_authority(target, &scope)?;
        let mut source = self.observe_private_metadata(scope.clone(), name, maximum)?;
        let mut output = create_file(&target.claim.directory, target_name)?;
        private_file(&output)?;
        let mut hash = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 65536];
        checkpoint(0);
        loop {
            source.verify()?;
            private_authority(target, &scope)?;
            private_binding(target, target_name, &output)?;
            let count = source.file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(count as u64)
                .filter(|total| *total <= source.stamp.bytes)
                .ok_or_else(read_changed)?;
            output.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            checkpoint(total);
        }
        if total != source.stamp.bytes {
            return Err(read_changed());
        }
        source.verify()?;
        output.sync_all()?;
        private_authority(target, &scope)?;
        private_binding(target, target_name, &output)?;
        target.claim.directory.sync_all()?;
        let stamp = ReadStamp::read(&output)?;
        if stamp.bytes != total {
            return Err(read_changed());
        }
        let expected = hash.finalize();
        let mut copied = MetadataCopy {
            source,
            target: MetadataObservation {
                root: target.clone(),
                scope,
                name: target_name.to_path_buf(),
                file: output,
                stamp,
            },
            sha256: format!("{expected:x}"),
        };
        copied.verify()?;
        let target_hash = copied
            .target
            .digest(&mut buffer, || copied.source.verify())?;
        copied.verify()?;
        if target_hash.as_slice() != expected.as_slice() {
            return Err(read_changed());
        }
        Ok(copied)
    }

    pub fn private_metadata_entries(
        &self,
        scope: &LiveRoots,
        maximum: usize,
    ) -> io::Result<Vec<(std::ffi::OsString, u64)>> {
        private_authority(self, scope)?;
        if maximum == 0 || maximum > 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Private metadata inventories require a limit between 1 and 256 files",
            ));
        }
        let names = directory_names(&self.claim.directory, maximum)?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            private_authority(self, scope)?;
            let file = open_read_child(&self.claim.directory, Path::new(&name))?;
            private_file(&file)?;
            entries.push((name, file.metadata()?.len()));
        }
        private_authority(self, scope)?;
        Ok(entries)
    }

    pub fn available_private_bytes(&self, scope: &LiveRoots) -> io::Result<u64> {
        private_authority(self, scope)?;
        let available = available_bytes(&self.claim.directory)?;
        private_authority(self, scope)?;
        Ok(available)
    }

    pub fn read_private_metadata(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        maximum: u64,
    ) -> io::Result<MetadataFile> {
        private_authority(self, &scope)?;
        let content = read_child_checked(
            &self.claim.directory,
            name,
            maximum,
            || private_authority(self, &scope),
            |_, _| {},
        )?;
        private_file(&content.file)?;
        let held = MetadataFile {
            root: self.clone(),
            scope,
            name: name.to_path_buf(),
            content: content.into(),
        };
        held.verify()?;
        Ok(held)
    }

    pub fn write_new_private_metadata(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        bytes: Arc<Vec<u8>>,
    ) -> io::Result<MetadataFile> {
        self.write_new_private_metadata_checked(scope, name, bytes, |_, _| Ok(()))
    }

    pub fn stage_new_private_metadata(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        primary: &Path,
        bytes: Arc<Vec<u8>>,
    ) -> io::Result<MetadataFile> {
        validate_filename(primary)?;
        private_absence(self, &scope, primary)?;
        self.write_new_private_metadata_checked(scope.clone(), name, bytes, |index, _| {
            if index == 0 {
                private_absence(self, &scope, primary)?;
            }
            Ok(())
        })
    }

    fn write_new_private_metadata_checked(
        &self,
        scope: Arc<LiveRoots>,
        name: &Path,
        bytes: Arc<Vec<u8>>,
        mut checkpoint: impl FnMut(usize, &File) -> io::Result<()>,
    ) -> io::Result<MetadataFile> {
        validate_filename(name)?;
        if bytes.len() as u64 > super::MAXIMUM_READ_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Private metadata exceeds its file bound",
            ));
        }
        private_authority(self, &scope)?;
        let mut file = create_file(&self.claim.directory, name)?;
        private_file(&file)?;
        checkpoint(0, &file)?;
        for (index, chunk) in bytes.chunks(65536).enumerate() {
            private_authority(self, &scope)?;
            private_binding(self, name, &file)?;
            file.write_all(chunk)?;
            checkpoint(index + 1, &file)?;
        }
        private_authority(self, &scope)?;
        private_binding(self, name, &file)?;
        file.sync_all()?;
        private_authority(self, &scope)?;
        self.claim.directory.sync_all()?;
        private_authority(self, &scope)?;
        let stamp = ReadStamp::read(&file)?;
        if stamp.bytes != bytes.len() as u64 {
            return Err(read_changed());
        }
        let held = MetadataFile {
            root: self.clone(),
            scope,
            name: name.to_path_buf(),
            content: SourceContent { bytes, file, stamp },
        };
        held.verify()?;
        Ok(held)
    }
}

fn private_authority(root: &DirectoryLease, scope: &LiveRoots) -> io::Result<()> {
    if !root.claim.private || !scope.contains(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Private metadata requires its retained scoped private directory",
        ));
    }
    scope.verify()?;
    Ok(())
}

fn private_absence(root: &DirectoryLease, scope: &LiveRoots, name: &Path) -> io::Result<()> {
    private_authority(root, scope)?;
    match open_read_child(&root.claim.directory, name) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Primary metadata already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    private_authority(root, scope)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_new(directory: &File, old: &Path, new: &Path) -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let old = std::ffi::CString::new(old.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let new = std::ffi::CString::new(new.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            directory.as_raw_fd(),
            old.as_ptr(),
            directory.as_raw_fd(),
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            old.as_ptr(),
            directory.as_raw_fd(),
            new.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_new(_directory: &File, _old: &Path, _new: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Private atomic publication needs a qualified no-replace rename implementation",
    ))
}

fn private_binding(root: &DirectoryLease, name: &Path, file: &File) -> io::Result<()> {
    private_file(file)?;
    let current = open_read_child(&root.claim.directory, name)?;
    private_file(&current)?;
    if !same_handles(file, &current)? {
        return Err(read_changed());
    }
    Ok(())
}

#[cfg(unix)]
fn private_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Private metadata file ownership or permissions changed",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn private_file(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Private metadata needs a qualified platform permission implementation",
    ))
}

#[cfg(unix)]
fn create_file(directory: &File, name: &Path) -> io::Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn create_file(_directory: &File, _name: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Private metadata creation needs a qualified platform permission implementation",
    ))
}

#[cfg(unix)]
fn available_bytes(directory: &File) -> io::Result<u64> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    if stat.f_frsize == 0 {
        return Err(io::Error::other(
            "Private metadata free space is unavailable",
        ));
    }
    (u128::from(stat.f_bavail) * u128::from(stat.f_frsize))
        .try_into()
        .map_err(|_| io::Error::other("Private metadata free space exceeds its accounting range"))
}

#[cfg(not(unix))]
fn available_bytes(_directory: &File) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Private metadata free space needs a qualified platform implementation",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn directory_names(directory: &File, maximum: usize) -> io::Result<Vec<std::ffi::OsString>> {
    use std::{
        ffi::{CStr, OsString},
        os::{fd::AsRawFd, unix::ffi::OsStringExt},
    };
    struct Entries(*mut libc::DIR);
    impl Drop for Entries {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    #[cfg(target_os = "linux")]
    fn errno() -> *mut libc::c_int {
        unsafe { libc::__errno_location() }
    }
    #[cfg(target_os = "macos")]
    fn errno() -> *mut libc::c_int {
        unsafe { libc::__error() }
    }

    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let entries = unsafe { libc::fdopendir(fd) };
    if entries.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    let entries = Entries(entries);
    let mut names = Vec::new();
    loop {
        unsafe {
            *errno() = 0;
        }
        let entry = unsafe { libc::readdir(entries.0) };
        if entry.is_null() {
            let code = unsafe { *errno() };
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." || name == super::CONTROL_FILE.as_bytes() {
            continue;
        }
        if names.len() >= maximum {
            return Err(io::Error::other(
                "Private metadata file inventory exceeds its bound",
            ));
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
    Ok(names)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn directory_names(_directory: &File, _maximum: usize) -> io::Result<Vec<std::ffi::OsString>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Private metadata inventory needs a qualified platform implementation",
    ))
}

fn matches_digest(digest: &[u8; 32], expected: &str) -> bool {
    let hex = b"0123456789abcdef";
    expected.len() == 64
        && digest
            .iter()
            .zip(expected.as_bytes().as_chunks::<2>().0)
            .all(|(byte, pair)| *pair == [hex[(byte >> 4) as usize], hex[(byte & 15) as usize]])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn fixture() -> (tempfile::TempDir, DirectoryLease, Arc<LiveRoots>) {
        let temporary = tempfile::tempdir().unwrap();
        let parent = DirectoryLease::acquire(temporary.path().join("config")).unwrap();
        let initial = LiveRoots::single(parent.clone()).unwrap();
        let private = parent
            .acquire_private_child(&initial, Path::new("private"), true)
            .unwrap();
        let scope = LiveRoots::new(vec![parent, private.clone()]).unwrap();
        (temporary, private, scope)
    }

    #[test]
    fn consumed_metadata_releases_raw_bytes_but_keeps_the_same_file_lock_and_exact_binding() {
        use std::os::fd::AsRawFd;
        let (_temporary, owner, scope) = fixture();
        let size = 2 * 1024 * 1024 + 3;
        let held = owner
            .write_new_private_metadata(scope, Path::new("index.json"), Arc::new(vec![b'x'; size]))
            .unwrap();
        held.try_lock_exclusive().unwrap();
        let fd = held.content.file.as_raw_fd();
        let raw = held.shared_bytes();
        let weak = Arc::downgrade(&raw);
        drop(raw);
        assert!(weak.upgrade().is_some());
        let observed = held.into_observation().unwrap();
        assert!(weak.upgrade().is_none());
        assert_eq!(observed.file.as_raw_fd(), fd);
        assert_eq!(observed.bytes(), size as u64);
        observed.verify().unwrap();
        let contender = File::open(owner.root().join("index.json")).unwrap();
        assert!(contender.try_lock().is_err());
        fs::rename(
            owner.root().join("index.json"),
            owner.root().join("retained.json"),
        )
        .unwrap();
        fs::write(owner.root().join("index.json"), b"foreign replacement").unwrap();
        fs::set_permissions(
            owner.root().join("index.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(observed.verify().is_err());
        assert_eq!(
            fs::read(owner.root().join("index.json")).unwrap(),
            b"foreign replacement"
        );
        assert_eq!(
            fs::metadata(owner.root().join("retained.json"))
                .unwrap()
                .len(),
            size as u64
        );
        drop(observed);
        crate::storage::after_test_release(owner.root(), || {
            contender.try_lock().map_err(crate::storage::lock_error)
        });
    }

    #[test]
    fn consumed_metadata_refuses_changed_contents_without_repair() {
        let (_temporary, owner, scope) = fixture();
        let held = owner
            .write_new_private_metadata(
                scope,
                Path::new("index.json"),
                Arc::new(b"original".to_vec()),
            )
            .unwrap();
        fs::write(owner.root().join("index.json"), b"changed data").unwrap();
        assert!(held.into_observation().is_err());
        assert_eq!(
            fs::read(owner.root().join("index.json")).unwrap(),
            b"changed data"
        );
    }

    #[test]
    fn streamed_copy_preserves_bytes_hash_and_private_ownership_without_payload_retention() {
        let (_source_temp, source, source_scope) = fixture();
        let (_target_temp, target, _) = fixture();
        let bytes = Arc::new(vec![b'p'; 196609]);
        let original = source
            .write_new_private_metadata(source_scope, Path::new("blob"), bytes.clone())
            .unwrap();
        let scope = LiveRoots::new(vec![source.clone(), target.clone()]).unwrap();
        let mut checkpoints = Vec::new();
        let copied = source
            .copy_new_private_metadata_checked(
                scope.clone(),
                Path::new("blob"),
                &target,
                Path::new("copy"),
                bytes.len() as u64,
                |bytes| checkpoints.push(bytes),
            )
            .unwrap();
        assert_eq!(checkpoints, [0, 65536, 131072, 196608, 196609]);
        assert_eq!(copied.bytes(), bytes.len() as u64);
        assert_eq!(
            copied.sha256(),
            format!("{:x}", Sha256::digest(bytes.as_slice()))
        );
        assert_eq!(
            fs::read(target.root().join("copy")).unwrap(),
            bytes.as_slice()
        );
        assert_eq!(
            fs::metadata(target.root().join("copy"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
        let source_path = source.root().to_path_buf();
        let target_path = target.root().to_path_buf();
        drop(original);
        drop(source);
        drop(target);
        drop(scope);
        assert_eq!(Arc::strong_count(&bytes), 1);
        copied.verify().unwrap();
        assert!(DirectoryLease::acquire(&source_path).is_err());
        assert!(DirectoryLease::acquire(&target_path).is_err());
        drop(copied);
        let _source = crate::storage::after_test_release(&source_path, || {
            DirectoryLease::acquire(&source_path)
        });
        let _target = crate::storage::after_test_release(&target_path, || {
            DirectoryLease::acquire(&target_path)
        });
    }

    #[test]
    fn streamed_copy_refuses_size_foreign_scope_symlink_and_existing_target_without_overwrite() {
        let (_source_temp, source, source_scope) = fixture();
        let (_target_temp, target, target_scope) = fixture();
        source
            .write_new_private_metadata(
                source_scope.clone(),
                Path::new("blob"),
                Arc::new(b"source".to_vec()),
            )
            .unwrap();
        target
            .write_new_private_metadata(
                target_scope,
                Path::new("existing"),
                Arc::new(b"preserved".to_vec()),
            )
            .unwrap();
        let scope = LiveRoots::new(vec![source.clone(), target.clone()]).unwrap();
        assert!(
            source
                .copy_new_private_metadata_to(
                    source_scope,
                    Path::new("blob"),
                    &target,
                    Path::new("foreign"),
                    64
                )
                .is_err()
        );
        assert!(
            source
                .copy_new_private_metadata_to(
                    scope.clone(),
                    Path::new("blob"),
                    &target,
                    Path::new("oversized"),
                    5
                )
                .is_err()
        );
        assert!(!target.root().join("oversized").exists());
        symlink("blob", source.root().join("link")).unwrap();
        assert!(
            source
                .copy_new_private_metadata_to(
                    scope.clone(),
                    Path::new("link"),
                    &target,
                    Path::new("symlink"),
                    64
                )
                .is_err()
        );
        assert!(!target.root().join("symlink").exists());
        assert_eq!(
            source
                .copy_new_private_metadata_to(
                    scope,
                    Path::new("blob"),
                    &target,
                    Path::new("existing"),
                    64
                )
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fs::read(target.root().join("existing")).unwrap(),
            b"preserved"
        );
    }

    #[test]
    fn streamed_copy_detects_midstream_mutation_and_preserves_the_incomplete_target() {
        let (_source_temp, source, source_scope) = fixture();
        let (_target_temp, target, _) = fixture();
        source
            .write_new_private_metadata(
                source_scope,
                Path::new("blob"),
                Arc::new(vec![b'a'; 131073]),
            )
            .unwrap();
        let scope = LiveRoots::new(vec![source.clone(), target.clone()]).unwrap();
        let result = source.copy_new_private_metadata_checked(
            scope,
            Path::new("blob"),
            &target,
            Path::new("copy"),
            131073,
            |bytes| {
                if bytes == 65536 {
                    fs::write(source.root().join("blob"), vec![b'b'; 131073]).unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(target.root().join("copy")).unwrap(),
            vec![b'a'; 65536]
        );
        assert_eq!(
            fs::read(source.root().join("blob")).unwrap(),
            vec![b'b'; 131073]
        );
    }

    #[test]
    fn streamed_copy_hashes_actual_target_bytes_before_returning_a_receipt() {
        let (_source_temp, source, source_scope) = fixture();
        let (_target_temp, target, _) = fixture();
        source
            .write_new_private_metadata(
                source_scope,
                Path::new("blob"),
                Arc::new(vec![b'a'; 65536]),
            )
            .unwrap();
        let scope = LiveRoots::new(vec![source.clone(), target.clone()]).unwrap();
        let result = source.copy_new_private_metadata_checked(
            scope,
            Path::new("blob"),
            &target,
            Path::new("copy"),
            65536,
            |bytes| {
                if bytes == 65536 {
                    fs::write(target.root().join("copy"), vec![b'z'; 65536]).unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(target.root().join("copy")).unwrap(),
            vec![b'z'; 65536]
        );
        assert_eq!(
            fs::read(source.root().join("blob")).unwrap(),
            vec![b'a'; 65536]
        );
    }

    #[test]
    fn streamed_copy_receipt_rejects_later_content_or_path_replacement() {
        let (_source_temp, source, source_scope) = fixture();
        let (_target_temp, target, _) = fixture();
        source
            .write_new_private_metadata(
                source_scope,
                Path::new("blob"),
                Arc::new(b"source".to_vec()),
            )
            .unwrap();
        let scope = LiveRoots::new(vec![source.clone(), target.clone()]).unwrap();
        let copied = source
            .copy_new_private_metadata_to(scope, Path::new("blob"), &target, Path::new("copy"), 64)
            .unwrap();
        fs::rename(target.root().join("copy"), target.root().join("retained")).unwrap();
        fs::write(target.root().join("copy"), b"source").unwrap();
        fs::set_permissions(
            target.root().join("copy"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(copied.verify().is_err());
        assert_eq!(fs::read(target.root().join("retained")).unwrap(), b"source");
    }

    #[test]
    fn private_name_observation_bounds_mixed_names_without_granting_file_authority() {
        let (_temporary, root, scope) = fixture();
        root.acquire_private_child(&scope, Path::new("child"), true)
            .unwrap();
        symlink("missing", root.root().join("link")).unwrap();
        assert!(root.private_entry_names(&scope, 1).is_err());
        let mut names = root.private_entry_names(&scope, 2).unwrap();
        names.sort();
        assert_eq!(names, ["child", "link"]);
        assert!(root.private_metadata_entries(&scope, 2).is_err());
        assert!(root.private_entry_names(&scope, 513).is_err());
        assert!(root.private_entry_names(&scope, 0).is_err());
        assert!(
            root.root()
                .join("link")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn staged_private_write_never_writes_payload_through_a_primary_alias() {
        let (_temporary, root, scope) = fixture();
        let error = root
            .stage_new_private_metadata(
                scope.clone(),
                Path::new("alias"),
                Path::new("alias"),
                Arc::new(b"must stay private".to_vec()),
            )
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(fs::read(root.root().join("alias")).unwrap().is_empty());
        root.write_new_private_metadata(
            scope.clone(),
            Path::new("primary"),
            Arc::new(b"preserved".to_vec()),
        )
        .unwrap();
        assert!(
            root.stage_new_private_metadata(
                scope,
                Path::new("stage"),
                Path::new("primary"),
                Arc::new(b"candidate".to_vec())
            )
            .is_err()
        );
        assert!(!root.root().join("stage").exists());
        assert_eq!(fs::read(root.root().join("primary")).unwrap(), b"preserved");
    }

    #[test]
    fn staged_private_write_keeps_primary_absent_and_shares_payload_until_publication() {
        let (_temporary, root, scope) = fixture();
        let bytes = Arc::new(vec![b'p'; 131073]);
        let staged = root
            .stage_new_private_metadata(
                scope.clone(),
                Path::new("stage"),
                Path::new("primary"),
                bytes.clone(),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&staged.shared_bytes(), &bytes));
        assert!(!root.root().join("primary").exists());
        let observed = root
            .observe_private_metadata(scope, Path::new("stage"), bytes.len() as u64)
            .unwrap();
        let mut published = observed.publish_new(Path::new("primary")).unwrap();
        published
            .verify_digest(&format!("{:x}", Sha256::digest(bytes.as_slice())))
            .unwrap();
        assert_eq!(
            fs::read(root.root().join("primary")).unwrap(),
            bytes.as_slice()
        );
    }

    #[test]
    fn private_publication_moves_complete_bytes_and_refuses_replacement() {
        let (_temporary, root, scope) = fixture();
        let bytes = Arc::new(b"complete payload".to_vec());
        root.write_new_private_metadata(scope.clone(), Path::new("stage"), bytes.clone())
            .unwrap();
        let observed = root
            .observe_private_metadata(scope.clone(), Path::new("stage"), 1024)
            .unwrap();
        let mut published = observed.publish_new(Path::new("primary")).unwrap();
        published
            .verify_digest(&format!("{:x}", Sha256::digest(bytes.as_slice())))
            .unwrap();
        assert!(!root.root().join("stage").exists());
        assert_eq!(
            fs::read(root.root().join("primary")).unwrap(),
            bytes.as_slice()
        );
        root.write_new_private_metadata(
            scope.clone(),
            Path::new("next"),
            Arc::new(b"candidate".to_vec()),
        )
        .unwrap();
        assert_eq!(
            root.observe_private_metadata(scope, Path::new("next"), 1024)
                .unwrap()
                .publish_new(Path::new("primary"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(root.root().join("next")).unwrap(), b"candidate");
        assert_eq!(
            fs::read(root.root().join("primary")).unwrap(),
            bytes.as_slice()
        );
    }

    #[test]
    fn private_publication_refuses_changed_content_and_replaced_root_before_rename() {
        let (temporary, root, scope) = fixture();
        root.write_new_private_metadata(
            scope.clone(),
            Path::new("stage"),
            Arc::new(b"original".to_vec()),
        )
        .unwrap();
        let observed = root
            .observe_private_metadata(scope.clone(), Path::new("stage"), 1024)
            .unwrap();
        fs::write(root.root().join("stage"), b"changed!").unwrap();
        assert!(observed.publish_new(Path::new("primary")).is_err());
        assert!(!root.root().join("primary").exists());
        let observed = root
            .observe_private_metadata(scope, Path::new("stage"), 1024)
            .unwrap();
        let moved = temporary.path().join("retained");
        fs::rename(root.root(), &moved).unwrap();
        fs::create_dir(root.root()).unwrap();
        fs::write(root.root().join("primary"), b"foreign").unwrap();
        assert!(observed.publish_new(Path::new("primary")).is_err());
        assert_eq!(fs::read(moved.join("stage")).unwrap(), b"changed!");
        assert_eq!(fs::read(root.root().join("primary")).unwrap(), b"foreign");
    }

    #[test]
    fn private_file_is_exclusive_private_before_content_and_retained_after_owner_drop() {
        let (_temporary, root, scope) = fixture();
        let bytes = Arc::new(vec![b'p'; 131073]);
        let held = root
            .write_new_private_metadata_checked(
                scope.clone(),
                Path::new("blob"),
                bytes.clone(),
                |index, file| {
                    assert_eq!(
                        file.metadata().unwrap().permissions().mode() & 0o7777,
                        0o600
                    );
                    if index == 0 {
                        assert_eq!(file.metadata().unwrap().len(), 0);
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(Arc::ptr_eq(&held.shared_bytes(), &bytes));
        assert_eq!(held.bytes(), bytes.as_slice());
        assert_eq!(
            root.write_new_private_metadata(scope.clone(), Path::new("blob"), bytes.clone())
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let reopened = root
            .read_private_metadata(scope.clone(), Path::new("blob"), bytes.len() as u64)
            .unwrap();
        assert_eq!(reopened.bytes(), bytes.as_slice());
        assert!(
            root.read_private_metadata(scope.clone(), Path::new("blob"), bytes.len() as u64 - 1)
                .is_err()
        );
        let path = root.root().to_path_buf();
        drop(root);
        drop(scope);
        held.verify().unwrap();
        assert!(DirectoryLease::acquire(&path).is_err());
        drop(held);
        drop(reopened);
        assert_eq!(fs::read(path.join("blob")).unwrap(), bytes.as_slice());
    }

    #[test]
    fn changed_permissions_content_and_symlinks_are_refused_without_repair_or_deletion() {
        let (_temporary, root, scope) = fixture();
        let held = root
            .write_new_private_metadata(
                scope.clone(),
                Path::new("blob"),
                Arc::new(b"original".to_vec()),
            )
            .unwrap();
        fs::set_permissions(root.root().join("blob"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(held.verify().is_err());
        assert!(
            root.read_private_metadata(scope.clone(), Path::new("blob"), 128)
                .is_err()
        );
        assert_eq!(
            fs::metadata(root.root().join("blob"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
        fs::set_permissions(root.root().join("blob"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(held.verify().is_err());
        fs::write(root.root().join("blob"), b"changed").unwrap();
        assert!(held.verify().is_err());
        symlink("blob", root.root().join("link")).unwrap();
        assert!(
            root.read_private_metadata(scope.clone(), Path::new("link"), 128)
                .is_err()
        );
        assert!(
            root.write_new_private_metadata(scope, Path::new("link"), Arc::new(vec![]))
                .is_err()
        );
        assert_eq!(fs::read(root.root().join("blob")).unwrap(), b"changed");
    }

    #[test]
    fn lost_root_retains_partial_private_bytes_and_never_writes_into_replacement() {
        let (temporary, root, scope) = fixture();
        let moved = temporary.path().join("retained");
        let bytes = Arc::new(vec![b'x'; 131073]);
        let failure = root
            .write_new_private_metadata_checked(scope, Path::new("blob"), bytes, |index, _| {
                if index == 1 {
                    fs::rename(root.root(), &moved).unwrap();
                    fs::create_dir(root.root()).unwrap();
                    fs::write(root.root().join("sentinel"), b"foreign").unwrap();
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert!(super::super::is_root_unavailable(&failure));
        assert_eq!(fs::metadata(moved.join("blob")).unwrap().len(), 65536);
        assert!(!root.root().join("blob").exists());
        assert_eq!(fs::read(root.root().join("sentinel")).unwrap(), b"foreign");
    }

    #[test]
    fn private_inventory_is_bounded_repeatable_and_checks_every_entry_without_following_links() {
        let (_temporary, root, scope) = fixture();
        assert!(root.private_metadata_entries(&scope, 8).unwrap().is_empty());
        let _available = root.available_private_bytes(&scope).unwrap();
        root.write_new_private_metadata(
            scope.clone(),
            Path::new("first"),
            Arc::new(b"one".to_vec()),
        )
        .unwrap();
        root.write_new_private_metadata(
            scope.clone(),
            Path::new("second"),
            Arc::new(b"two".to_vec()),
        )
        .unwrap();
        assert!(root.private_metadata_entries(&scope, 1).is_err());
        let mut before = root.private_metadata_entries(&scope, 2).unwrap();
        before.sort();
        let mut after = root.private_metadata_entries(&scope, 2).unwrap();
        after.sort();
        assert_eq!(before, after);
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|(_, bytes)| *bytes == 3));
        symlink("first", root.root().join("third")).unwrap();
        assert!(root.private_metadata_entries(&scope, 8).is_err());
        assert_eq!(fs::read(root.root().join("first")).unwrap(), b"one");
    }
}
