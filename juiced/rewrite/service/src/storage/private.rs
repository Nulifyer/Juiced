use super::{DirectoryLease, LiveRoots};
use std::{io, path::Path};

impl DirectoryLease {
    pub fn acquire_private_child(
        &self,
        scope: &LiveRoots,
        name: &Path,
        create: bool,
    ) -> io::Result<DirectoryLease> {
        #[cfg(unix)]
        {
            self.acquire_private_child_checked(scope, name, create, |_, _| {})
        }
        #[cfg(not(unix))]
        {
            let _ = (scope, name, create);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Private metadata directory acquisition requires a qualified platform implementation",
            ))
        }
    }
}

pub(super) fn verify_claim(claim: &super::Claim) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::verify_claim(claim)
    }
    #[cfg(not(unix))]
    {
        let _ = claim;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Private metadata ownership is unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::storage::{
        Ancestor, Claim, RootUnavailable, same_handles, try_exclusive, validate_filename,
    };
    use std::{
        ffi::{CStr, CString},
        fs::{File, Metadata},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        sync::Arc,
    };

    #[derive(Clone, Copy, Eq, PartialEq)]
    pub(super) enum Point {
        BeforeDirectory,
        BeforeMkdir,
        DirectoryOpened,
        ControlOpened,
    }

    impl DirectoryLease {
        pub(super) fn acquire_private_child_checked(
            &self,
            scope: &LiveRoots,
            name: &Path,
            create: bool,
            mut checkpoint: impl FnMut(Point, &File),
        ) -> io::Result<DirectoryLease> {
            if !scope.contains(self) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The parent directory lease is outside this storage scope",
                ));
            }
            validate_filename(name)?;
            let name = CString::new(name.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            let root = self
                .root()
                .join(Path::new(std::ffi::OsStr::from_bytes(name.to_bytes())));
            let mut registry = self
                .owner
                .claims
                .lock()
                .map_err(|_| io::Error::other("Directory ownership state is unavailable"))?;
            registry.retain(|claim| claim.strong_count() != 0);
            let existing: Vec<_> = registry
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .collect();
            let verify = || -> io::Result<()> {
                scope.verify()?;
                for claim in &existing {
                    claim
                        .verify()
                        .map_err(|source| RootUnavailable { source })?;
                }
                Ok(())
            };
            verify()?;
            if existing
                .iter()
                .any(|claim| claim.root != root && claim.root.starts_with(&root))
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "A broader private root cannot replace live narrower ownership",
                ));
            }
            checkpoint(Point::BeforeDirectory, &self.claim.directory);
            let before = match entry(&self.claim.directory, &name) {
                Ok(stat) => Some(stat),
                Err(error) if error.kind() == io::ErrorKind::NotFound && create => None,
                Err(error) => return Err(error),
            };
            if let Some(before) = &before {
                private_stat(before, true)?;
            } else {
                verify()?;
                checkpoint(Point::BeforeMkdir, &self.claim.directory);
                if unsafe { libc::mkdirat(self.claim.directory.as_raw_fd(), name.as_ptr(), 0o700) }
                    < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            let directory = open_directory(&self.claim.directory, &name)?;
            private_metadata(&directory.metadata()?, true)?;
            if let Some(before) = &before {
                same_entry(before, &directory, true)?;
            }
            checkpoint(Point::DirectoryOpened, &directory);
            verify()?;
            binding(&self.claim.directory, &name, &directory, true)?;
            if let Some(held) = existing.iter().find(|claim| claim.root == root) {
                if !held.private {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Live generic directory ownership cannot be upgraded to private ownership",
                    ));
                }
                if !same_handles(&held.directory, &directory)? {
                    return Err(changed());
                }
                private_metadata(&held._control.metadata()?, false)?;
                let control_bytes = control_name();
                let control_name =
                    CStr::from_bytes_with_nul(&control_bytes).expect("static control name");
                let current = open_control(&directory, control_name, false)?;
                if !same_handles(&held._control, &current)? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "The private control identity changed",
                    ));
                }
                verify()?;
                binding(&self.claim.directory, &name, &directory, true)?;
                binding(&directory, control_name, &held._control, false)?;
                return Ok(DirectoryLease {
                    owner: self.owner.clone(),
                    claim: held.clone(),
                });
            }
            try_exclusive(&directory)?;
            verify()?;
            binding(&self.claim.directory, &name, &directory, true)?;
            let control_bytes = control_name();
            let control_name =
                CStr::from_bytes_with_nul(&control_bytes).expect("static control name");
            let control = open_control(&directory, control_name, create)?;
            checkpoint(Point::ControlOpened, &control);
            verify()?;
            binding(&self.claim.directory, &name, &directory, true)?;
            binding(&directory, control_name, &control, false)?;
            try_exclusive(&control)?;
            verify()?;
            binding(&self.claim.directory, &name, &directory, true)?;
            binding(&directory, control_name, &control, false)?;
            let claim = Arc::new(Claim {
                root,
                directory,
                _control: control,
                ancestors: vec![Ancestor::Owned(self.claim.clone())],
                private: true,
            });
            registry.push(Arc::downgrade(&claim));
            Ok(DirectoryLease {
                owner: self.owner.clone(),
                claim,
            })
        }
    }

    pub(super) fn verify_claim(claim: &Claim) -> io::Result<()> {
        private_metadata(&claim.directory.metadata()?, true)?;
        private_metadata(&claim._control.metadata()?, false)?;
        let control_bytes = control_name();
        let name = CStr::from_bytes_with_nul(&control_bytes).expect("static control name");
        binding(&claim.directory, name, &claim._control, false)
    }

    fn control_name() -> [u8; crate::storage::CONTROL_FILE.len() + 1] {
        let mut name = [0; crate::storage::CONTROL_FILE.len() + 1];
        name[..crate::storage::CONTROL_FILE.len()]
            .copy_from_slice(crate::storage::CONTROL_FILE.as_bytes());
        name
    }

    fn changed() -> io::Error {
        RootUnavailable {
            source: io::Error::other("The private directory binding changed"),
        }
        .into()
    }

    fn private_stat(stat: &libc::stat, directory: bool) -> io::Result<()> {
        let expected = if directory {
            libc::S_IFDIR
        } else {
            libc::S_IFREG
        };
        if stat.st_mode & libc::S_IFMT != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Private metadata requires regular control files and directories",
            ));
        }
        private_owner_mode(u64::from(stat.st_uid), u64::from(stat.st_mode))?;
        if !directory && stat.st_nlink != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The private control file must have one link",
            ));
        }
        Ok(())
    }

    fn private_owner_mode(uid: u64, mode: u64) -> io::Result<()> {
        if uid != u64::from(unsafe { libc::geteuid() }) || mode & 0o7077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Private metadata requires its effective owner, no group or other access, and no special permission bits",
            ));
        }
        Ok(())
    }

    #[test]
    fn owner_validation_refuses_an_observed_foreign_uid_even_with_private_modes() {
        let uid = u64::from(unsafe { libc::geteuid() });
        assert!(private_owner_mode(uid, 0o700).is_ok());
        assert_eq!(
            private_owner_mode(uid + 1, 0o700).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    fn private_metadata(metadata: &Metadata, directory: bool) -> io::Result<()> {
        if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        private_owner_mode(u64::from(metadata.uid()), u64::from(metadata.mode()))?;
        if !directory && metadata.nlink() != 1 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(())
    }

    fn entry(directory: &File, name: &CStr) -> io::Result<libc::stat> {
        let mut stat = std::mem::MaybeUninit::uninit();
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { stat.assume_init() })
    }

    // Unix dev_t and ino_t widths vary; std exposes both as u64.
    #[allow(clippy::unnecessary_cast)]
    fn same_entry(stat: &libc::stat, file: &File, directory: bool) -> io::Result<()> {
        let held = file.metadata()?;
        if held.dev() != stat.st_dev as u64 || held.ino() != stat.st_ino as u64 {
            return Err(if directory {
                changed()
            } else {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The private control identity changed",
                )
            });
        }
        Ok(())
    }

    fn binding(parent: &File, name: &CStr, file: &File, directory: bool) -> io::Result<()> {
        let current = entry(parent, name)?;
        private_stat(&current, directory)?;
        private_metadata(&file.metadata()?, directory)?;
        same_entry(&current, file, directory)
    }

    fn open_directory(parent: &File, name: &CStr) -> io::Result<File> {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_control(directory: &File, name: &CStr, create: bool) -> io::Result<File> {
        let before = match entry(directory, name) {
            Ok(stat) => {
                private_stat(&stat, false)?;
                Some(stat)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => None,
            Err(error) => return Err(error),
        };
        let flags = libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if before.is_some() {
                libc::O_RDONLY
            } else {
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL
            };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        private_metadata(&file.metadata()?, false)?;
        if let Some(before) = &before {
            same_entry(before, &file, false)?;
        }
        binding(directory, name, &file, false)?;
        Ok(file)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{unix::Point, *};
    use crate::storage::{CONTROL_FILE, is_root_unavailable};
    use std::{
        fs::{self, OpenOptions},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
        process::Command,
        sync::Arc,
    };

    const CHILD_ROOT: &str = "JUICED_PRIVATE_DIRECTORY_TEST_ROOT";

    fn fixture() -> (tempfile::TempDir, DirectoryLease, Arc<LiveRoots>) {
        let directory = tempfile::tempdir().unwrap();
        let parent = DirectoryLease::acquire(directory.path().join("config")).unwrap();
        let scope = LiveRoots::single(parent.clone()).unwrap();
        (directory, parent, scope)
    }

    fn private_directory(path: &Path) {
        fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }

    fn control(path: &Path) {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
    }

    fn error(result: io::Result<DirectoryLease>) -> io::Error {
        result.err().expect("private acquisition must refuse")
    }

    #[test]
    fn umask_subprocess() {
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            return;
        };
        unsafe {
            libc::umask(0);
        }
        let parent = DirectoryLease::acquire(root).unwrap();
        let scope = LiveRoots::single(parent.clone()).unwrap();
        let mut checked_directory = false;
        let mut checked_control = false;
        let lease = parent
            .acquire_private_child_checked(&scope, Path::new("private"), true, |point, file| {
                match point {
                    Point::DirectoryOpened => {
                        assert_eq!(file.metadata().unwrap().mode() & 0o7777, 0o700);
                        checked_directory = true;
                    }
                    Point::ControlOpened => {
                        assert_eq!(file.metadata().unwrap().mode() & 0o7777, 0o600);
                        checked_control = true;
                    }
                    _ => {}
                }
            })
            .unwrap();
        assert!(checked_directory && checked_control);
        assert_eq!(fs::metadata(lease.root()).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(
            fs::metadata(lease.root().join(CONTROL_FILE))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
        assert_eq!(fs::metadata(lease.root()).unwrap().uid(), unsafe {
            libc::geteuid()
        });
        assert!(lease.claim.private);
    }

    #[test]
    fn private_modes_are_applied_at_creation_under_a_permissive_isolated_umask() {
        let directory = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("storage::private::tests::umask_subprocess")
            .arg("--nocapture")
            .env(CHILD_ROOT, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            directory
                .path()
                .join("private")
                .join(CONTROL_FILE)
                .is_file()
        );
    }

    #[test]
    fn existing_only_missing_directory_or_control_never_creates_files() {
        let (_directory, parent, scope) = fixture();
        assert_eq!(
            error(parent.acquire_private_child(&scope, Path::new("absent"), false)).kind(),
            io::ErrorKind::NotFound
        );
        assert!(!parent.root().join("absent").exists());
        let child = parent.root().join("existing");
        private_directory(&child);
        assert_eq!(
            error(parent.acquire_private_child(&scope, Path::new("existing"), false)).kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read_dir(child).unwrap().count(), 0);
        assert_eq!(fs::read_dir(parent.root()).unwrap().count(), 2);
    }

    #[test]
    fn names_and_scope_membership_reject_before_creating_a_child() {
        let (directory, parent, scope) = fixture();
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "./child",
            "child/",
            "child/nested",
            CONTROL_FILE,
        ] {
            assert_eq!(
                error(parent.acquire_private_child(&scope, Path::new(name), true)).kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let other = DirectoryLease::acquire(directory.path().join("other")).unwrap();
        let other_scope = LiveRoots::single(other).unwrap();
        assert_eq!(
            error(parent.acquire_private_child(&other_scope, Path::new("child"), true)).kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read_dir(parent.root()).unwrap().count(), 1);
    }

    #[test]
    fn permissive_and_special_modes_refuse_without_repair() {
        let (_directory, parent, scope) = fixture();
        for mode in [0o755, 0o770, 0o1700, 0o2700, 0o4700] {
            let name = format!("dir-{mode}");
            let child = parent.root().join(&name);
            private_directory(&child);
            fs::set_permissions(&child, fs::Permissions::from_mode(mode)).unwrap();
            let before = fs::metadata(&child).unwrap().mode();
            assert_eq!(
                error(parent.acquire_private_child(&scope, Path::new(&name), true)).kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(fs::metadata(&child).unwrap().mode(), before);
            assert_eq!(fs::read_dir(child).unwrap().count(), 0);
        }
        for mode in [0o644, 0o660, 0o1600, 0o2600, 0o4600] {
            let name = format!("file-{mode}");
            let child = parent.root().join(&name);
            private_directory(&child);
            let marker = child.join(CONTROL_FILE);
            control(&marker);
            fs::write(&marker, b"retained control bytes").unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(mode)).unwrap();
            let before = fs::metadata(&marker).unwrap().mode();
            assert_eq!(
                error(parent.acquire_private_child(&scope, Path::new(&name), false)).kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(fs::metadata(&marker).unwrap().mode(), before);
            assert_eq!(fs::read(marker).unwrap(), b"retained control bytes");
        }
    }

    #[test]
    fn control_symlinks_hardlinks_and_special_files_do_not_gain_ownership() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let (directory, parent, scope) = fixture();
        let outside = directory.path().join("outside");
        private_directory(&outside);
        fs::write(outside.join("secret"), b"foreign bytes").unwrap();
        symlink(&outside, parent.root().join("linked-directory")).unwrap();
        assert!(
            parent
                .acquire_private_child(&scope, Path::new("linked-directory"), true)
                .is_err()
        );
        assert!(!outside.join(CONTROL_FILE).exists());
        for name in ["symlink", "hardlink", "fifo"] {
            let child = parent.root().join(name);
            private_directory(&child);
            let marker = child.join(CONTROL_FILE);
            match name {
                "symlink" => symlink(outside.join("secret"), &marker).unwrap(),
                "hardlink" => {
                    control(&marker);
                    fs::hard_link(&marker, child.join("second-link")).unwrap();
                }
                "fifo" => {
                    let name = CString::new(marker.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            assert_eq!(
                error(parent.acquire_private_child(&scope, Path::new(name), false)).kind(),
                io::ErrorKind::InvalidData
            );
        }
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"foreign bytes");
    }

    #[test]
    fn same_owner_reuse_keeps_exact_claim_and_ancestor_ownership_without_a_gap() {
        let (_directory, parent, scope) = fixture();
        let child = parent
            .acquire_private_child(&scope, Path::new("private"), true)
            .unwrap();
        let reused = parent
            .acquire_private_child(&scope, Path::new("private"), false)
            .unwrap();
        assert!(Arc::ptr_eq(&child.claim, &reused.claim));
        assert!(
            crate::storage::same_handles(&child.claim.directory, &reused.claim.directory).unwrap()
        );
        assert!(DirectoryLease::acquire(child.root()).is_err());
        let parent_claim = Arc::downgrade(&parent.claim);
        drop(scope);
        drop(parent);
        assert!(parent_claim.upgrade().is_some());
        child.verify().unwrap();
        drop(reused);
        drop(child);
        assert!(parent_claim.upgrade().is_none());
    }

    #[test]
    fn a_live_generic_claim_cannot_be_promoted_or_released_by_private_acquisition() {
        let (_directory, parent, scope) = fixture();
        let path = parent.root().join("private");
        private_directory(&path);
        control(&path.join(CONTROL_FILE));
        let generic = parent.claim(&path).unwrap();
        assert!(!generic.claim.private);
        assert_eq!(
            error(parent.acquire_private_child(&scope, Path::new("private"), false)).kind(),
            io::ErrorKind::WouldBlock
        );
        generic.verify().unwrap();
        generic
            .write_json("still-owned.json", &serde_json::json!({"retained":true}))
            .unwrap();
        assert!(DirectoryLease::acquire(&path).is_err());
        let retained = Arc::downgrade(&generic.claim);
        drop(generic);
        assert!(retained.upgrade().is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let private = loop {
            match parent.acquire_private_child(&scope, Path::new("private"), false) {
                Ok(private) => break private,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(error) => {
                    panic!("released generic ownership did not permit private acquisition: {error}")
                }
            }
        };
        assert!(private.claim.private);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(path.join("still-owned.json")).unwrap()
            )
            .unwrap(),
            serde_json::json!({"retained":true})
        );
    }

    #[test]
    fn a_broader_private_acquisition_cannot_upgrade_live_narrower_ownership() {
        let (_directory, parent, scope) = fixture();
        let child_path = parent.root().join("private");
        private_directory(&child_path);
        let nested = parent.claim(child_path.join("nested")).unwrap();
        assert_eq!(
            error(parent.acquire_private_child(&scope, Path::new("private"), true)).kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(!child_path.join(CONTROL_FILE).exists());
        nested.verify().unwrap();
    }

    #[test]
    fn loss_of_parent_or_another_scoped_root_rejects_before_directory_creation() {
        for loss in ["parent-replaced", "parent-missing", "other-replaced"] {
            let (directory, parent, initial) = fixture();
            let other = parent.claim(directory.path().join("other")).unwrap();
            let scope = LiveRoots::new(vec![parent.clone(), other.clone()]).unwrap();
            drop(initial);
            let target = if loss == "other-replaced" {
                other.root()
            } else {
                parent.root()
            };
            let retired = directory.path().join("retired");
            let result = parent.acquire_private_child_checked(
                &scope,
                Path::new("private"),
                true,
                |point, _| {
                    if point == Point::BeforeDirectory {
                        if loss == "parent-missing" {
                            fs::remove_dir_all(target).unwrap();
                        } else {
                            fs::rename(target, &retired).unwrap();
                            fs::create_dir(target).unwrap();
                            fs::write(target.join("foreign"), b"untouched").unwrap();
                        }
                    }
                },
            );
            assert!(is_root_unavailable(&error(result)));
            assert!(!parent.root().join("private").exists());
            if loss != "parent-missing" {
                assert_eq!(fs::read(target.join("foreign")).unwrap(), b"untouched");
                assert_eq!(fs::read_dir(target).unwrap().count(), 1);
                assert!(!retired.join("private").exists());
            }
        }
    }

    #[test]
    fn anchored_mkdir_during_parent_replacement_never_touches_the_new_root() {
        let (directory, parent, scope) = fixture();
        let retired = directory.path().join("retired");
        let result =
            parent.acquire_private_child_checked(&scope, Path::new("private"), true, |point, _| {
                if point == Point::BeforeMkdir {
                    fs::rename(parent.root(), &retired).unwrap();
                    fs::create_dir(parent.root()).unwrap();
                    fs::write(parent.root().join("foreign"), b"untouched").unwrap();
                }
            });
        assert!(is_root_unavailable(&error(result)));
        assert!(!parent.root().join("private").exists());
        assert_eq!(
            fs::read(parent.root().join("foreign")).unwrap(),
            b"untouched"
        );
        assert_eq!(fs::read_dir(retired.join("private")).unwrap().count(), 0);
        assert_eq!(
            fs::metadata(retired.join("private")).unwrap().mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn child_replacement_before_control_creation_preserves_both_directories() {
        let (directory, parent, scope) = fixture();
        let child = parent.root().join("private");
        let retired = directory.path().join("retired-child");
        let result =
            parent.acquire_private_child_checked(&scope, Path::new("private"), true, |point, _| {
                if point == Point::DirectoryOpened {
                    fs::rename(&child, &retired).unwrap();
                    private_directory(&child);
                    fs::write(child.join("foreign"), b"untouched").unwrap();
                }
            });
        assert!(is_root_unavailable(&error(result)));
        assert_eq!(fs::read_dir(&retired).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&child).unwrap().count(), 1);
        assert_eq!(fs::read(child.join("foreign")).unwrap(), b"untouched");
    }

    #[test]
    fn control_replacement_before_locking_is_not_mistaken_for_root_identity_loss() {
        let (_directory, parent, scope) = fixture();
        let child = parent.root().join("private");
        let marker = child.join(CONTROL_FILE);
        let result =
            parent.acquire_private_child_checked(&scope, Path::new("private"), true, |point, _| {
                if point == Point::ControlOpened {
                    fs::rename(&marker, child.join("retired-control")).unwrap();
                    control(&marker);
                    fs::write(&marker, b"foreign control").unwrap();
                }
            });
        let error = error(result);
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!is_root_unavailable(&error));
        assert_eq!(fs::read(marker).unwrap(), b"foreign control");
        assert_eq!(fs::read(child.join("retired-control")).unwrap(), b"");
    }

    #[test]
    fn lifetime_checks_reject_permission_control_identity_and_unlink_changes() {
        for change in [
            "directory-mode",
            "control-mode",
            "control-identity",
            "control-missing",
            "control-hardlink",
        ] {
            let (_directory, parent, bootstrap) = fixture();
            let child = parent
                .acquire_private_child(&bootstrap, Path::new("private"), true)
                .unwrap();
            let scope = LiveRoots::new(vec![parent.clone(), child.clone()]).unwrap();
            let marker = child.root().join(CONTROL_FILE);
            match change {
                "directory-mode" => {
                    fs::set_permissions(child.root(), fs::Permissions::from_mode(0o755)).unwrap()
                }
                "control-mode" => {
                    fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap()
                }
                "control-identity" => {
                    fs::rename(&marker, child.root().join("retired-control")).unwrap();
                    control(&marker);
                }
                "control-missing" => fs::remove_file(&marker).unwrap(),
                "control-hardlink" => {
                    fs::hard_link(&marker, child.root().join("second-link")).unwrap()
                }
                _ => unreachable!(),
            }
            let before = fs::read_dir(parent.root()).unwrap().count();
            let failure: io::Error = scope.verify().unwrap_err().into();
            assert!(is_root_unavailable(&failure));
            assert!(is_root_unavailable(&child.verify().unwrap_err()));
            assert!(is_root_unavailable(&error(parent.acquire_private_child(
                &bootstrap,
                Path::new("private"),
                false
            ))));
            assert!(
                scope
                    .write_json(&parent, "blocked.json", &serde_json::json!(true))
                    .unwrap_err()
                    .root_unavailable()
            );
            assert_eq!(fs::read_dir(parent.root()).unwrap().count(), before);
            assert!(!parent.root().join("blocked.json").exists());
        }
    }

    #[test]
    fn readonly_owner_only_modes_are_observed_without_repair() {
        let (_directory, parent, scope) = fixture();
        let child = parent
            .acquire_private_child(&scope, Path::new("private"), true)
            .unwrap();
        let marker = child.root().join(CONTROL_FILE);
        fs::set_permissions(child.root(), fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o400)).unwrap();
        child.verify().unwrap();
        let reused = parent
            .acquire_private_child(&scope, Path::new("private"), false)
            .unwrap();
        assert!(Arc::ptr_eq(&child.claim, &reused.claim));
        assert_eq!(fs::metadata(child.root()).unwrap().mode() & 0o7777, 0o500);
        assert_eq!(fs::metadata(&marker).unwrap().mode() & 0o7777, 0o400);
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(child.root(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}
