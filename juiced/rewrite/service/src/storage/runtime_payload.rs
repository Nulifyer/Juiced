use super::{
    DirectoryLease, DirectoryReader, LiveRoots, ReadStamp, read_changed, same_handles,
    validate_filename,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub(crate) struct Member<'a> {
    pub path: &'a Path,
    pub bytes: u64,
    pub sha256: &'a str,
    pub executable: bool,
}

pub(crate) struct RuntimePayload {
    owner: DirectoryLease,
    scope: Arc<LiveRoots>,
    directory: File,
}

struct Child {
    parent: File,
    name: PathBuf,
    file: File,
}
impl Child {
    fn verify(&self) -> io::Result<()> {
        let current = open(
            &self.parent,
            &self.name,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        private_directory(&self.file)?;
        if !same_handles(&self.file, &current)? {
            return Err(read_changed());
        }
        Ok(())
    }
}

fn open(parent: &File, name: &Path, flags: i32, mode: u32) -> io::Result<File> {
    validate_filename(name)?;
    let name = CString::new(name.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn private_directory(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || ![0o700, 0o500].contains(&(metadata.mode() & 0o7777))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Runtime payload directory ownership changed",
        ));
    }
    Ok(())
}
fn private_file(file: &File, mode: Option<u32>) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || mode.map_or_else(
            || ![0o600, 0o500, 0o400].contains(&(metadata.mode() & 0o7777)),
            |mode| metadata.mode() & 0o7777 != mode,
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Runtime payload file ownership changed",
        ));
    }
    Ok(())
}
fn child(parent: &File, name: &Path, create: bool) -> io::Result<Child> {
    validate_filename(name)?;
    if create {
        let text = CString::new(name.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), text.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        } else {
            parent.sync_all()?;
        }
    }
    let result = Child {
        parent: parent.try_clone()?,
        name: name.to_owned(),
        file: open(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
    };
    result.verify()?;
    Ok(result)
}
fn components(path: &Path) -> io::Result<Vec<&Path>> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(Path::new(name)),
            _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
        })
        .collect::<io::Result<Vec<_>>>()?;
    if parts.is_empty()
        || parts.len() > 32
        || path.as_os_str().len() > 4096
        || parts
            .iter()
            .any(|name| name.as_os_str().as_bytes().starts_with(b".juiced-"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Runtime member path is outside its bounds",
        ));
    }
    Ok(parts)
}

impl RuntimePayload {
    pub(crate) fn open(
        owner: DirectoryLease,
        scope: Arc<LiveRoots>,
        create: bool,
    ) -> io::Result<Self> {
        if !scope.contains(&owner) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        scope.verify()?;
        let payload = child(&owner.claim.directory, Path::new("payload"), create)?;
        let result = Self {
            owner,
            scope,
            directory: payload.file,
        };
        result.verify()?;
        Ok(result)
    }
    pub(crate) fn path(&self) -> PathBuf {
        self.owner.root().join("payload")
    }
    pub(crate) fn verify(&self) -> io::Result<()> {
        self.scope.verify()?;
        let current = open(
            &self.owner.claim.directory,
            Path::new("payload"),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        private_directory(&self.directory)?;
        if !same_handles(&self.directory, &current)? {
            return Err(read_changed());
        }
        self.scope.verify()?;
        Ok(())
    }
    fn parents(&self, parts: &[&Path], create: bool) -> io::Result<Vec<Child>> {
        let mut directories = Vec::with_capacity(parts.len());
        for name in parts {
            let parent = directories
                .last()
                .map(|directory: &Child| &directory.file)
                .unwrap_or(&self.directory);
            directories.push(child(parent, name, create)?);
        }
        Ok(directories)
    }
    pub(crate) fn copy_member(
        &self,
        source: &DirectoryReader,
        member: Member<'_>,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<bool> {
        let parts = components(member.path)?;
        self.verify()?;
        source.verify()?;
        check()?;
        let mut source_dirs = Vec::with_capacity(parts.len() - 1);
        let target_dirs = self.parents(&parts[..parts.len() - 1], true)?;
        for name in &parts[..parts.len() - 1] {
            let source_parent = source_dirs
                .last()
                .map(|directory: &Child| &directory.file)
                .unwrap_or(source.directory());
            let source_file = open(source_parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
            source_dirs.push(Child {
                parent: source_parent.try_clone()?,
                name: (*name).to_owned(),
                file: source_file,
            });
        }
        let source_parent = source_dirs
            .last()
            .map(|directory| &directory.file)
            .unwrap_or(source.directory());
        let target_parent = target_dirs
            .last()
            .map(|directory| &directory.file)
            .unwrap_or(&self.directory);
        let name = parts[parts.len() - 1];
        let mut input = open(source_parent, name, libc::O_RDONLY, 0)?;
        let metadata = input.metadata()?;
        if !metadata.is_file() || metadata.len() != member.bytes {
            return Err(read_changed());
        }
        let source_stamp = ReadStamp::read(&input)?;
        let (mut output, reused) = match open(
            target_parent,
            name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        ) {
            Ok(file) => (file, false),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                (open(target_parent, name, libc::O_RDONLY, 0)?, true)
            }
            Err(error) => return Err(error),
        };
        private_file(&output, None)?;
        let existing_stamp = ReadStamp::read(&output)?;
        if reused && existing_stamp.bytes != member.bytes {
            return Err(read_changed());
        }
        let verify = |input: &File, output: &File| -> io::Result<()> {
            self.verify()?;
            source.verify()?;
            for directory in &target_dirs {
                directory.verify()?;
            }
            for directory in &source_dirs {
                let current = open(
                    &directory.parent,
                    &directory.name,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )?;
                if !same_handles(&directory.file, &current)? {
                    return Err(read_changed());
                }
            }
            let current = open(target_parent, name, libc::O_RDONLY, 0)?;
            let source_current = open(source_parent, name, libc::O_RDONLY, 0)?;
            if !same_handles(output, &current)? || ReadStamp::read(input)? != source_stamp {
                return Err(read_changed());
            }
            if !same_handles(input, &source_current)? {
                return Err(read_changed());
            }
            Ok(())
        };
        let mut hash = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            check()?;
            verify(&input, &output)?;
            let count = if reused {
                output.read(&mut buffer)?
            } else {
                input.read(&mut buffer)?
            };
            if count == 0 {
                break;
            }
            total = total
                .checked_add(count as u64)
                .filter(|bytes| *bytes <= member.bytes)
                .ok_or_else(read_changed)?;
            if !reused {
                output.write_all(&buffer[..count])?;
            }
            hash.update(&buffer[..count]);
        }
        check()?;
        verify(&input, &output)?;
        if total != member.bytes || format!("{:x}", hash.finalize()) != member.sha256 {
            return Err(read_changed());
        }
        if reused && ReadStamp::read(&output)? != existing_stamp {
            return Err(read_changed());
        }
        if !reused {
            output.sync_all()?;
            let written_stamp = ReadStamp::read(&output)?;
            output.rewind()?;
            let mut target_hash = Sha256::new();
            let mut observed = 0u64;
            loop {
                check()?;
                verify(&input, &output)?;
                let count = output.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                observed = observed
                    .checked_add(count as u64)
                    .filter(|bytes| *bytes <= member.bytes)
                    .ok_or_else(read_changed)?;
                target_hash.update(&buffer[..count]);
            }
            if observed != member.bytes
                || ReadStamp::read(&output)? != written_stamp
                || format!("{:x}", target_hash.finalize()) != member.sha256
            {
                return Err(read_changed());
            }
        }
        let mode = if member.executable { 0o500 } else { 0o400 };
        output.set_permissions(std::fs::Permissions::from_mode(mode))?;
        output.sync_all()?;
        target_parent.sync_all()?;
        private_file(&output, Some(mode))?;
        verify(&input, &output)?;
        Ok(reused)
    }
    pub(crate) fn prepare_library_directories(&self, paths: &[PathBuf]) -> io::Result<()> {
        self.verify()?;
        for path in paths {
            if path == Path::new(".") {
                continue;
            }
            let parts = components(path)?;
            let directories = self.parents(&parts, true)?;
            for directory in &directories {
                directory.verify()?;
            }
        }
        self.verify()
    }
    pub(crate) fn seal<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> io::Result<()> {
        self.verify()?;
        let mut paths: Vec<_> = paths
            .into_iter()
            .filter(|path| !path.as_os_str().is_empty() && *path != Path::new("."))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        for path in paths {
            let parts = components(path)?;
            let directories = self.parents(&parts, false)?;
            for directory in directories.into_iter().rev() {
                directory.verify()?;
                directory
                    .file
                    .set_permissions(std::fs::Permissions::from_mode(0o500))?;
                directory.file.sync_all()?;
            }
        }
        self.directory
            .set_permissions(std::fs::Permissions::from_mode(0o500))?;
        self.directory.sync_all()?;
        self.owner.claim.directory.sync_all()?;
        self.verify()
    }
    pub(crate) fn verify_sealed<'a>(
        &self,
        members: impl IntoIterator<Item = Member<'a>>,
        library_directories: &[PathBuf],
    ) -> io::Result<()> {
        self.verify()?;
        if self.directory.metadata()?.mode() & 0o7777 != 0o500 {
            return Err(read_changed());
        }
        for member in members {
            let parts = components(member.path)?;
            let directories = self.parents(&parts[..parts.len() - 1], false)?;
            let parent = directories
                .last()
                .map(|directory| &directory.file)
                .unwrap_or(&self.directory);
            let file = open(parent, parts[parts.len() - 1], libc::O_RDONLY, 0)?;
            private_file(&file, Some(if member.executable { 0o500 } else { 0o400 }))?;
            if file.metadata()?.len() != member.bytes {
                return Err(read_changed());
            }
            for directory in &directories {
                directory.verify()?;
                if directory.file.metadata()?.mode() & 0o7777 != 0o500 {
                    return Err(read_changed());
                }
            }
            self.verify()?;
        }
        for path in library_directories {
            if path == Path::new(".") {
                continue;
            }
            let parts = components(path)?;
            for directory in self.parents(&parts, false)? {
                directory.verify()?;
                if directory.file.metadata()?.mode() & 0o7777 != 0o500 {
                    return Err(read_changed());
                }
            }
        }
        self.verify()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, RuntimePayload) {
        let directory = tempfile::tempdir().unwrap();
        let parent = DirectoryLease::acquire(directory.path()).unwrap();
        let bootstrap = LiveRoots::single(parent.clone()).unwrap();
        let owner = parent
            .acquire_private_child(&bootstrap, Path::new("package"), true)
            .unwrap();
        let scope = LiveRoots::new(vec![parent, owner.clone()]).unwrap();
        (directory, RuntimePayload::open(owner, scope, true).unwrap())
    }
    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn streamed_nested_runtime_files_verify_actual_bytes_reuse_and_seal_without_source_changes() {
        let (_directory, payload) = fixture();
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("lib")).unwrap();
        let bytes = vec![71u8; 180000];
        fs::write(source.path().join("lib/engine"), &bytes).unwrap();
        let reader = DirectoryReader::open_existing(source.path()).unwrap();
        let hash = digest(&bytes);
        let member = || Member {
            path: Path::new("lib/engine"),
            bytes: bytes.len() as u64,
            sha256: &hash,
            executable: true,
        };
        assert!(!payload.copy_member(&reader, member(), || Ok(())).unwrap());
        assert_eq!(fs::read(payload.path().join("lib/engine")).unwrap(), bytes);
        assert_eq!(fs::read(source.path().join("lib/engine")).unwrap(), bytes);
        assert!(payload.copy_member(&reader, member(), || Ok(())).unwrap());
        payload.seal([Path::new("lib")]).unwrap();
        payload.verify_sealed([member()], &[]).unwrap();
        for path in [
            payload.path(),
            payload.path().join("lib"),
            payload.path().join("lib/engine"),
        ] {
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o7777, 0o500);
        }
        assert!(!payload.path().join(super::super::CONTROL_FILE).exists());
        assert!(
            payload
                .owner
                .root()
                .join(super::super::CONTROL_FILE)
                .exists()
        );
        payload.verify().unwrap();
        open(
            &payload.directory,
            Path::new("lib"),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )
        .unwrap()
        .set_permissions(fs::Permissions::from_mode(0o700))
        .unwrap();
        payload
            .directory
            .set_permissions(fs::Permissions::from_mode(0o700))
            .unwrap();
    }

    #[test]
    fn interrupted_copy_retains_private_partial_bytes_and_never_overwrites_them() {
        let (_directory, payload) = fixture();
        let source = tempfile::tempdir().unwrap();
        let bytes = vec![13u8; 180000];
        fs::write(source.path().join("engine"), &bytes).unwrap();
        let reader = DirectoryReader::open_existing(source.path()).unwrap();
        let hash = digest(&bytes);
        let member = || Member {
            path: Path::new("engine"),
            bytes: bytes.len() as u64,
            sha256: &hash,
            executable: true,
        };
        let mut calls = 0;
        assert!(
            payload
                .copy_member(&reader, member(), || {
                    calls += 1;
                    if calls == 3 {
                        Err(io::Error::from(io::ErrorKind::Interrupted))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        let retained = fs::read(payload.path().join("engine")).unwrap();
        assert_eq!(retained.len(), 65536);
        assert_eq!(
            fs::metadata(payload.path().join("engine")).unwrap().mode() & 0o7777,
            0o600
        );
        assert!(payload.copy_member(&reader, member(), || Ok(())).is_err());
        assert_eq!(fs::read(payload.path().join("engine")).unwrap(), retained);
    }

    #[test]
    fn runtime_copy_refuses_traversal_links_and_foreign_existing_files() {
        let (_directory, payload) = fixture();
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("engine"), b"expected").unwrap();
        let reader = DirectoryReader::open_existing(source.path()).unwrap();
        let hash = digest(b"expected");
        for path in [
            "../outside",
            "/absolute",
            ".juiced-part",
            "lib/../../outside",
        ] {
            assert!(
                payload
                    .copy_member(
                        &reader,
                        Member {
                            path: Path::new(path),
                            bytes: 8,
                            sha256: &hash,
                            executable: true
                        },
                        || Ok(())
                    )
                    .is_err()
            );
        }
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("engine"), b"foreign!").unwrap();
        std::os::unix::fs::symlink(outside.path(), payload.path().join("lib")).unwrap();
        fs::create_dir(source.path().join("lib")).unwrap();
        fs::write(source.path().join("lib/engine"), b"expected").unwrap();
        assert!(
            payload
                .copy_member(
                    &reader,
                    Member {
                        path: Path::new("lib/engine"),
                        bytes: 8,
                        sha256: &hash,
                        executable: true
                    },
                    || Ok(())
                )
                .is_err()
        );
        assert_eq!(
            fs::read(outside.path().join("engine")).unwrap(),
            b"foreign!"
        );
        fs::write(payload.path().join("engine"), b"foreign!").unwrap();
        fs::set_permissions(
            payload.path().join("engine"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(
            payload
                .copy_member(
                    &reader,
                    Member {
                        path: Path::new("engine"),
                        bytes: 8,
                        sha256: &hash,
                        executable: true
                    },
                    || Ok(())
                )
                .is_err()
        );
        assert_eq!(
            fs::read(payload.path().join("engine")).unwrap(),
            b"foreign!"
        );
    }

    #[test]
    fn moved_payload_refuses_new_writes_and_preserves_the_foreign_replacement() {
        let (_directory, payload) = fixture();
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("engine"), b"expected").unwrap();
        let reader = DirectoryReader::open_existing(source.path()).unwrap();
        fs::rename(payload.path(), payload.owner.root().join("retained")).unwrap();
        fs::create_dir(payload.path()).unwrap();
        fs::set_permissions(payload.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(payload.path().join("foreign"), b"keep").unwrap();
        assert!(
            payload
                .copy_member(
                    &reader,
                    Member {
                        path: Path::new("engine"),
                        bytes: 8,
                        sha256: &digest(b"expected"),
                        executable: true
                    },
                    || Ok(())
                )
                .is_err()
        );
        assert_eq!(fs::read(payload.path().join("foreign")).unwrap(), b"keep");
        assert!(!payload.path().join("engine").exists());
    }

    #[test]
    fn runtime_copy_hashes_actual_output_and_refuses_midstream_target_corruption() {
        let (_directory, payload) = fixture();
        let source = tempfile::tempdir().unwrap();
        let bytes = vec![41u8; 180000];
        fs::write(source.path().join("engine"), &bytes).unwrap();
        let reader = DirectoryReader::open_existing(source.path()).unwrap();
        let mut calls = 0;
        assert!(
            payload
                .copy_member(
                    &reader,
                    Member {
                        path: Path::new("engine"),
                        bytes: bytes.len() as u64,
                        sha256: &digest(&bytes),
                        executable: true
                    },
                    || {
                        calls += 1;
                        if calls == 3 {
                            let mut file = fs::OpenOptions::new()
                                .write(true)
                                .open(payload.path().join("engine"))?;
                            file.write_all(&[99])?;
                        }
                        Ok(())
                    }
                )
                .is_err()
        );
        assert_eq!(
            fs::metadata(payload.path().join("engine")).unwrap().mode() & 0o7777,
            0o600
        );
        assert_eq!(fs::read(payload.path().join("engine")).unwrap()[0], 99);
    }
}
