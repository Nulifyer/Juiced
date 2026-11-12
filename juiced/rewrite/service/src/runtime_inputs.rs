use crate::{config::Provider, storage::same_file};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata, OpenOptions},
    path::{Component, Path, PathBuf},
    time::{Instant, SystemTime},
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Absent,
    EmptyRegular,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Summary {
    pub role: Role,
    pub condition: Condition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Refusal {
    pub code: &'static str,
    pub role: Option<Role>,
    pub reason: &'static str,
}

impl Refusal {
    fn new(role: Option<Role>, reason: &'static str) -> Self {
        Self {
            code: "unbound_runtime_config",
            role,
            reason,
        }
    }
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn public_message(&self) -> &'static str {
        "External runtime configuration cannot be bound to this launch."
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} ({:?}): {}",
            self.code, self.role, self.reason
        )
    }
}
impl std::error::Error for Refusal {}

pub struct ConfigurationInputs {
    observations: Vec<Observation>,
    fingerprint: String,
}

impl std::fmt::Debug for ConfigurationInputs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigurationInputs")
            .field("inputs", &self.observations.len())
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

struct Directory {
    path: PathBuf,
    file: File,
}

enum RetainedCondition {
    Absent {
        missing: PathBuf,
        boundary_stamp: Stamp,
    },
    Empty {
        file: File,
        stamp: Stamp,
    },
}

struct Observation {
    role: Role,
    path: PathBuf,
    directories: Vec<Directory>,
    condition: RetainedCondition,
}

impl ConfigurationInputs {
    pub fn for_profile(
        adapter: Provider,
        command: &str,
        environment: &BTreeMap<OsString, OsString>,
        deadline: Instant,
    ) -> Result<Self, Refusal> {
        if adapter != Provider::LlamaCpp || crate::runtime_arguments::fixture_command(command) {
            return Self::inspect(Vec::new(), deadline);
        }
        Self::inspect(candidate_paths(environment, deadline)?, deadline)
    }

    /// This is a metadata point observation, never a writer fence or immutable qualification.
    /// No contents are read; deadlines cannot interrupt blocked kernel metadata I/O.
    pub fn inspect(candidates: Vec<(Role, PathBuf)>, deadline: Instant) -> Result<Self, Refusal> {
        check(deadline, None)?;
        if candidates.len() > 8 {
            return Err(Refusal::new(None, "candidate_limit"));
        }
        let mut candidates = candidates
            .into_iter()
            .map(|(role, path)| {
                normalize(&path)
                    .map(|path| (role, path))
                    .map_err(|_| Refusal::new(Some(role), "invalid_candidate_path"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        candidates.sort();
        if candidates.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Refusal::new(None, "duplicate_candidate"));
        }
        let mut observations = Vec::with_capacity(candidates.len());
        for (role, path) in candidates {
            check(deadline, Some(role))?;
            observations.push(observe(role, path, deadline)?);
        }
        let mut hash = Sha256::new();
        hash.update(b"juiced-runtime-configuration-v1\0");
        hash.update((observations.len() as u64).to_le_bytes());
        for observation in &observations {
            hash.update([match observation.role {
                Role::System => 0,
                Role::User => 1,
            }]);
            hash_path(&mut hash, &observation.path);
            hash.update([match observation.condition {
                RetainedCondition::Absent { .. } => 0,
                RetainedCondition::Empty { .. } => 1,
            }]);
        }
        let result = Self {
            observations,
            fingerprint: format!("{:x}", hash.finalize()),
        };
        result.verify_fresh(deadline)?;
        Ok(result)
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// An absent boundary also stales on sibling publication; stamps do not enter the fingerprint.
    pub fn verify_fresh(&self, deadline: Instant) -> Result<(), Refusal> {
        check(deadline, None)?;
        for observation in &self.observations {
            observation.verify(deadline)?;
        }
        check(deadline, None)
    }

    pub fn summary(&self) -> Vec<Summary> {
        self.observations
            .iter()
            .map(|observation| Summary {
                role: observation.role,
                condition: match observation.condition {
                    RetainedCondition::Absent { .. } => Condition::Absent,
                    RetainedCondition::Empty { .. } => Condition::EmptyRegular,
                },
            })
            .collect()
    }
}

fn candidate_paths(
    environment: &BTreeMap<OsString, OsString>,
    deadline: Instant,
) -> Result<Vec<(Role, PathBuf)>, Refusal> {
    check(deadline, None)?;
    #[cfg(unix)]
    {
        // llama.cpp reads this system path independently of HOME/XDG overrides.
        let system = PathBuf::from("/etc/llama.cpp/config.ini");
        let user = match environment
            .get(OsStr::new("XDG_CONFIG_HOME"))
            .filter(|value| !value.is_empty())
        {
            Some(value) => environment_directory(value, Role::User)?,
            None => {
                let home = environment
                    .get(OsStr::new("HOME"))
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| Refusal::new(Some(Role::User), "missing_environment"))?;
                environment_directory(home, Role::User)?.join(".config")
            }
        };
        Ok(vec![
            (Role::System, system),
            (Role::User, user.join("llama.cpp/config.ini")),
        ])
    }
    #[cfg(windows)]
    {
        let system = windows_environment(environment, "PROGRAMDATA", Role::System, deadline)?;
        let user = windows_environment(environment, "APPDATA", Role::User, deadline)?;
        Ok(vec![
            (Role::System, system.join("llama.cpp/config.ini")),
            (Role::User, user.join("llama.cpp/config.ini")),
        ])
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = environment;
        Err(Refusal::new(None, "unsupported_configuration_platform"))
    }
}

fn environment_directory(value: &OsStr, role: Role) -> Result<PathBuf, Refusal> {
    if value.len() > 32768
        || value.is_empty()
        || !Path::new(value).is_absolute()
        || Path::new(value)
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Refusal::new(Some(role), "invalid_environment_path"));
    }
    Ok(PathBuf::from(value))
}

#[cfg(windows)]
fn windows_environment(
    environment: &BTreeMap<OsString, OsString>,
    name: &str,
    role: Role,
    deadline: Instant,
) -> Result<PathBuf, Refusal> {
    let mut found = None;
    for (key, value) in environment {
        check(deadline, Some(role))?;
        if key
            .to_str()
            .is_some_and(|key| key.eq_ignore_ascii_case(name))
        {
            if found.is_some_and(|prior| prior != value) {
                return Err(Refusal::new(Some(role), "ambiguous_environment"));
            }
            found = Some(value);
        }
    }
    environment_directory(
        found.ok_or_else(|| Refusal::new(Some(role), "missing_environment"))?,
        role,
    )
}

fn check(deadline: Instant, role: Option<Role>) -> Result<(), Refusal> {
    if Instant::now() >= deadline {
        Err(Refusal::new(role, "observation_deadline"))
    } else {
        Ok(())
    }
}

fn normalize(path: &Path) -> Result<PathBuf, ()> {
    if !path.is_absolute() || path.as_os_str().len() > 32768 || path.components().count() > 128 {
        return Err(());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component)
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(()),
        }
    }
    if !matches!(
        normalized.components().next_back(),
        Some(Component::Normal(_))
    ) {
        return Err(());
    }
    #[cfg(windows)]
    if !matches!(normalized.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(());
    }
    Ok(normalized)
}

fn observe(role: Role, path: PathBuf, deadline: Instant) -> Result<Observation, Refusal> {
    let failed = || Refusal::new(Some(role), "configuration_unavailable_or_nonempty");
    let mut current = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => current.push(component),
            Component::Normal(name) => names.push(name.to_owned()),
            _ => return Err(failed()),
        }
    }
    let root = open_root(&current).map_err(|_| failed())?;
    verify_type(&root.metadata().map_err(|_| failed())?, true).map_err(|_| failed())?;
    let mut directories = vec![Directory {
        path: current.clone(),
        file: root,
    }];
    for (index, name) in names.iter().enumerate() {
        check(deadline, Some(role))?;
        let child = current.join(name);
        let leaf = index + 1 == names.len();
        let parent = directories.last().ok_or_else(failed)?;
        match fs::symlink_metadata(&child) {
            Ok(metadata) => {
                verify_type(&metadata, !leaf).map_err(|_| failed())?;
                if leaf && metadata.len() != 0 {
                    return Err(failed());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let stamp = Stamp::read(&parent.file).map_err(|_| failed())?;
                let observation = Observation {
                    role,
                    path,
                    directories,
                    condition: RetainedCondition::Absent {
                        missing: child,
                        boundary_stamp: stamp,
                    },
                };
                observation.verify(deadline)?;
                return Ok(observation);
            }
            Err(_) => return Err(failed()),
        }
        let file = open_child(parent, &child, name, !leaf).map_err(|_| failed())?;
        verify_file(&file, &child, !leaf).map_err(|_| failed())?;
        if leaf {
            let stamp = Stamp::read(&file).map_err(|_| failed())?;
            if stamp.bytes != 0 {
                return Err(failed());
            }
            let observation = Observation {
                role,
                path,
                directories,
                condition: RetainedCondition::Empty { file, stamp },
            };
            observation.verify(deadline)?;
            return Ok(observation);
        }
        directories.push(Directory {
            path: child.clone(),
            file,
        });
        current = child;
    }
    Err(failed())
}

impl Observation {
    fn verify(&self, deadline: Instant) -> Result<(), Refusal> {
        let changed = || Refusal::new(Some(self.role), "configuration_changed");
        self.verify_directories(deadline)?;
        check(deadline, Some(self.role))?;
        match &self.condition {
            RetainedCondition::Empty { file, stamp } => {
                verify_file(file, &self.path, false).map_err(|_| changed())?;
                if Stamp::read(file).map_err(|_| changed())? != *stamp || stamp.bytes != 0 {
                    return Err(changed());
                }
            }
            RetainedCondition::Absent {
                missing,
                boundary_stamp,
            } => {
                let parent = self.directories.last().ok_or_else(changed)?;
                if Stamp::read(&parent.file).map_err(|_| changed())? != *boundary_stamp {
                    return Err(changed());
                }
                absent_child(parent, missing).map_err(|_| changed())?;
                if Stamp::read(&parent.file).map_err(|_| changed())? != *boundary_stamp {
                    return Err(changed());
                }
            }
        }
        self.verify_directories(deadline)?;
        check(deadline, Some(self.role))
    }

    fn verify_directories(&self, deadline: Instant) -> Result<(), Refusal> {
        for directory in &self.directories {
            check(deadline, Some(self.role))?;
            verify_file(&directory.file, &directory.path, true)
                .map_err(|_| Refusal::new(Some(self.role), "configuration_changed"))?;
        }
        Ok(())
    }
}

fn verify_type(metadata: &Metadata, directory: bool) -> std::io::Result<()> {
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(std::io::Error::other("Invalid metadata input"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::other("Reparse input"));
        }
    }
    Ok(())
}

fn verify_file(file: &File, path: &Path, directory: bool) -> std::io::Result<()> {
    verify_type(&file.metadata()?, directory)?;
    verify_type(&fs::symlink_metadata(path)?, directory)?;
    if !same_file(file, path)? {
        return Err(std::io::Error::other("Input identity changed"));
    }
    verify_type(&fs::symlink_metadata(path)?, directory)
}

#[derive(Eq, PartialEq)]
struct Stamp {
    bytes: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    changed: i64,
}

impl Stamp {
    fn read(file: &File) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            changed: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ctime(), metadata.ctime_nsec())
            },
            #[cfg(windows)]
            changed: windows_change(file)?,
        })
    }
}

#[cfg(unix)]
fn open_root(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(unix)]
fn open_child(
    parent: &Directory,
    _path: &Path,
    name: &std::ffi::OsStr,
    directory: bool,
) -> std::io::Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::other("Invalid component"))?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.file.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
fn absent_child(parent: &Directory, path: &Path) -> std::io::Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("Invalid missing component"))?;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::other("Invalid missing component"))?;
    let mut result = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            result.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
    {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "Missing component was published or is unknown",
        ))
    }
}

#[cfg(windows)]
fn open_root(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)
}

#[cfg(windows)]
fn open_child(
    _parent: &Directory,
    path: &Path,
    _name: &std::ffi::OsStr,
    directory: bool,
) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .share_mode(if directory { 3 } else { 7 })
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)
}

#[cfg(windows)]
fn absent_child(_parent: &Directory, path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(std::io::Error::other(
            "Missing component was published or is unknown",
        )),
    }
}

#[cfg(windows)]
fn windows_change(file: &File) -> std::io::Result<i64> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        creation: i64,
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
    let mut result = Basic::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            0,
            std::ptr::from_mut(&mut result).cast(),
            std::mem::size_of::<Basic>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(result.changed)
}

#[cfg(unix)]
fn hash_path(hash: &mut Sha256, path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    hash.update(b"unix\0");
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

#[cfg(windows)]
fn hash_path(hash: &mut Sha256, path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    hash.update(b"windows\0");
    hash.update((path.as_os_str().encode_wide().count() as u64).to_le_bytes());
    for value in path.as_os_str().encode_wide() {
        hash.update(value.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn open_root(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::other("Unsupported metadata platform"))
}
#[cfg(not(any(unix, windows)))]
fn open_child(
    _parent: &Directory,
    _path: &Path,
    _name: &std::ffi::OsStr,
    _directory: bool,
) -> std::io::Result<File> {
    Err(std::io::Error::other("Unsupported metadata platform"))
}
#[cfg(not(any(unix, windows)))]
fn absent_child(_parent: &Directory, _path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("Unsupported metadata platform"))
}
#[cfg(not(any(unix, windows)))]
fn hash_path(hash: &mut Sha256, path: &Path) {
    hash.update(path.as_os_str().as_encoded_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, time::Duration};

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        fs::create_dir(root.join("bound")).unwrap();
        (temporary, root)
    }
    fn inspect(path: &Path) -> ConfigurationInputs {
        ConfigurationInputs::inspect(vec![(Role::User, path.to_owned())], deadline()).unwrap()
    }
    fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, current: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    output.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        b"directory".to_vec(),
                    );
                    visit(root, &entry.path(), output);
                } else if kind.is_file() {
                    output.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        fs::read(entry.path()).unwrap(),
                    );
                } else if kind.is_symlink() {
                    output.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        b"symlink".to_vec(),
                    );
                } else {
                    output.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        b"special".to_vec(),
                    );
                }
            }
        }
        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }

    #[test]
    fn missing_candidates_and_empty_provider_inputs_do_not_create_anything() {
        let (_temporary, root) = fixture();
        let before = tree(&root);
        let inputs = inspect(&root.join("bound/missing/deeper/config.ini"));
        assert_eq!(inputs.summary()[0].condition, Condition::Absent);
        inputs.verify_fresh(deadline()).unwrap();
        assert_eq!(tree(&root), before);
        assert!(!root.join("bound/missing").exists());
        let empty = ConfigurationInputs::inspect(Vec::new(), deadline()).unwrap();
        assert!(empty.summary().is_empty());
        assert_eq!(empty.fingerprint().len(), 64);
        assert_eq!(
            empty.fingerprint(),
            ConfigurationInputs::inspect(Vec::new(), deadline())
                .unwrap()
                .fingerprint()
        );
    }

    #[test]
    fn empty_regular_files_are_retained_without_content_reads_or_mutation() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        fs::write(&path, b"").unwrap();
        let before = tree(&root);
        let inputs = inspect(&path);
        assert_eq!(inputs.summary()[0].condition, Condition::EmptyRegular);
        inputs.verify_fresh(deadline()).unwrap();
        assert_eq!(tree(&root), before);
        assert!(!format!("{inputs:?}").contains(&root.to_string_lossy().to_string()));
        let summary = serde_json::to_string(&inputs.summary()).unwrap();
        assert_eq!(summary, r#"[{"role":"user","condition":"empty_regular"}]"#);
    }

    #[test]
    fn nonempty_inputs_refuse_with_static_redacted_diagnostics() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/private-config-name.ini");
        fs::write(&path, b"private-key-do-not-read-or-report").unwrap();
        let before = tree(&root);
        let refusal =
            ConfigurationInputs::inspect(vec![(Role::System, path)], deadline()).unwrap_err();
        assert_eq!(refusal.code(), "unbound_runtime_config");
        assert_eq!(refusal.role, Some(Role::System));
        let output = format!(
            "{refusal:?} {refusal} {}",
            serde_json::to_string(&refusal).unwrap()
        );
        assert!(!output.contains("private-config-name") && !output.contains("private-key"));
        assert!(!output.contains(&root.to_string_lossy().to_string()));
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn traversal_relative_duplicate_and_excessive_candidates_refuse_without_creation() {
        let (_temporary, root) = fixture();
        let before = tree(&root);
        for path in [
            PathBuf::from("relative/config"),
            root.join("bound/../other"),
            PathBuf::from(std::path::MAIN_SEPARATOR_STR),
        ] {
            assert!(ConfigurationInputs::inspect(vec![(Role::User, path)], deadline()).is_err());
        }
        let deep = (0..129).fold(root.clone(), |path, _| path.join("missing"));
        assert!(ConfigurationInputs::inspect(vec![(Role::User, deep)], deadline()).is_err());
        assert!(
            ConfigurationInputs::inspect(
                vec![(Role::User, root.join("bound/config")); 9],
                deadline()
            )
            .is_err()
        );
        assert!(
            ConfigurationInputs::inspect(
                vec![(Role::User, root.join("bound/config")); 2],
                deadline()
            )
            .is_err()
        );
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn nonregular_parent_and_directory_leaf_are_not_absence() {
        let (_temporary, root) = fixture();
        fs::write(root.join("bound/not-a-directory"), b"").unwrap();
        let before = tree(&root);
        for path in [
            root.join("bound/not-a-directory/config"),
            root.join("bound"),
        ] {
            assert!(ConfigurationInputs::inspect(vec![(Role::User, path)], deadline()).is_err());
        }
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn candidate_publication_and_transient_publication_invalidate_absence() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        let inputs = inspect(&path);
        fs::write(&path, b"").unwrap();
        let before = tree(&root);
        assert_eq!(
            inputs.verify_fresh(deadline()).unwrap_err().reason,
            "configuration_changed"
        );
        assert_eq!(tree(&root), before);
        let present = inspect(&path);
        assert_ne!(inputs.fingerprint(), present.fingerprint());
        drop(present);
        fs::remove_file(&path).unwrap();
        assert!(inputs.verify_fresh(deadline()).is_err());
        let refreshed = inspect(&path);
        assert_eq!(inputs.fingerprint(), refreshed.fingerprint());
    }

    #[test]
    fn publication_of_a_missing_parent_refuses_even_before_a_leaf_exists() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/missing/config.ini");
        let inputs = inspect(&path);
        fs::create_dir(root.join("bound/missing")).unwrap();
        let before = tree(&root);
        assert!(inputs.verify_fresh(deadline()).is_err());
        assert_eq!(tree(&root), before);
        assert!(!path.exists());
    }

    #[test]
    fn only_the_nearest_missing_boundary_pins_directory_change_stamps() {
        let (_temporary, root) = fixture();
        fs::create_dir(root.join("outside")).unwrap();
        let inputs = inspect(&root.join("bound/config.ini"));
        fs::write(root.join("outside/other"), b"unrelated").unwrap();
        fs::write(root.join("outside-boundary"), b"unrelated").unwrap();
        inputs.verify_fresh(deadline()).unwrap();
        fs::write(
            root.join("bound/sibling"),
            b"unrelated-but-boundary-changed",
        )
        .unwrap();
        assert!(inputs.verify_fresh(deadline()).is_err());
    }

    #[test]
    fn empty_inode_replacement_is_stale_but_does_not_change_the_semantic_fingerprint() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        fs::write(&path, b"").unwrap();
        let original = inspect(&path);
        fs::rename(&path, root.join("bound/retired")).unwrap();
        fs::write(&path, b"").unwrap();
        assert!(original.verify_fresh(deadline()).is_err());
        let replacement = inspect(&path);
        assert_eq!(original.fingerprint(), replacement.fingerprint());
    }

    #[test]
    fn content_writes_and_truncation_back_to_empty_are_stale() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        fs::write(&path, b"").unwrap();
        let inputs = inspect(&path);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"new-unbound-settings").unwrap();
        assert!(inputs.verify_fresh(deadline()).is_err());
        fs::write(&path, b"").unwrap();
        File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let before = tree(&root);
        assert!(inputs.verify_fresh(deadline()).is_err());
        assert_eq!(tree(&root), before);
        assert_eq!(inputs.fingerprint(), inspect(&path).fingerprint());
    }

    #[test]
    fn fingerprints_bind_roles_and_paths_but_not_candidate_order() {
        let (_temporary, root) = fixture();
        fs::create_dir(root.join("second")).unwrap();
        let first = root.join("bound/config");
        let second = root.join("second/config");
        let left = ConfigurationInputs::inspect(
            vec![(Role::System, first.clone()), (Role::User, second.clone())],
            deadline(),
        )
        .unwrap();
        let reordered = ConfigurationInputs::inspect(
            vec![(Role::User, second.clone()), (Role::System, first.clone())],
            deadline(),
        )
        .unwrap();
        assert_eq!(left.fingerprint(), reordered.fingerprint());
        let role =
            ConfigurationInputs::inspect(vec![(Role::User, first.clone())], deadline()).unwrap();
        let other_path =
            ConfigurationInputs::inspect(vec![(Role::User, second)], deadline()).unwrap();
        assert_ne!(role.fingerprint(), other_path.fingerprint());
        assert_ne!(left.fingerprint(), role.fingerprint());
        let other_role =
            ConfigurationInputs::inspect(vec![(Role::System, first)], deadline()).unwrap();
        assert_ne!(role.fingerprint(), other_role.fingerprint());
    }

    #[test]
    fn expired_deadlines_never_authorize_observation_or_freshness() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        let inputs = inspect(&path);
        let before = tree(&root);
        let refusal =
            ConfigurationInputs::inspect(vec![(Role::User, path)], Instant::now()).unwrap_err();
        assert_eq!(refusal.reason, "observation_deadline");
        assert_eq!(
            inputs.verify_fresh(Instant::now()).unwrap_err().reason,
            "observation_deadline"
        );
        assert_eq!(tree(&root), before);
    }

    #[test]
    fn other_adapters_do_not_consult_llama_environment_or_configuration_paths() {
        let empty = ConfigurationInputs::inspect(Vec::new(), deadline()).unwrap();
        for adapter in [Provider::ExternalHttp, Provider::WhisperCpp] {
            let inputs = ConfigurationInputs::for_profile(
                adapter,
                "arbitrary-worker",
                &BTreeMap::new(),
                deadline(),
            )
            .unwrap();
            assert!(inputs.summary().is_empty());
            assert_eq!(inputs.fingerprint(), empty.fingerprint());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_candidates_use_only_the_captured_xdg_or_home_without_passwd_guessing() {
        let (_temporary, root) = fixture();
        let mut environment = BTreeMap::new();
        environment.insert(
            OsString::from("HOME"),
            root.join("private-home").into_os_string(),
        );
        environment.insert(
            OsString::from("XDG_CONFIG_HOME"),
            root.join("explicit-xdg").into_os_string(),
        );
        let paths = candidate_paths(&environment, deadline()).unwrap();
        assert_eq!(
            paths[0],
            (Role::System, PathBuf::from("/etc/llama.cpp/config.ini"))
        );
        assert_eq!(
            paths[1],
            (Role::User, root.join("explicit-xdg/llama.cpp/config.ini"))
        );
        environment.insert(OsString::from("XDG_CONFIG_HOME"), OsString::new());
        assert_eq!(
            candidate_paths(&environment, deadline()).unwrap()[1].1,
            root.join("private-home/.config/llama.cpp/config.ini")
        );
        environment.remove(OsStr::new("HOME"));
        assert_eq!(
            candidate_paths(&environment, deadline())
                .unwrap_err()
                .reason,
            "missing_environment"
        );
        environment.insert(
            OsString::from("XDG_CONFIG_HOME"),
            OsString::from("relative"),
        );
        assert_eq!(
            candidate_paths(&environment, deadline())
                .unwrap_err()
                .reason,
            "invalid_environment_path"
        );
        environment.insert(
            OsString::from("XDG_CONFIG_HOME"),
            root.join("bound/../elsewhere").into_os_string(),
        );
        assert_eq!(
            candidate_paths(&environment, deadline())
                .unwrap_err()
                .reason,
            "invalid_environment_path"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidates_require_unambiguous_captured_appdata_paths() {
        let mut environment = BTreeMap::new();
        environment.insert(
            OsString::from("ProgramData"),
            OsString::from(r"C:\ProgramData"),
        );
        environment.insert(
            OsString::from("AppData"),
            OsString::from(r"C:\Users\private\AppData\Roaming"),
        );
        let paths = candidate_paths(&environment, deadline()).unwrap();
        assert_eq!(
            paths[0].1,
            PathBuf::from(r"C:\ProgramData\llama.cpp\config.ini")
        );
        assert_eq!(
            paths[1].1,
            PathBuf::from(r"C:\Users\private\AppData\Roaming\llama.cpp\config.ini")
        );
        environment.remove(OsStr::new("ProgramData"));
        assert_eq!(
            candidate_paths(&environment, deadline()).unwrap_err().role,
            Some(Role::System)
        );
        environment.insert(
            OsString::from("PROGRAMDATA"),
            OsString::from(r"C:\ProgramData"),
        );
        environment.insert(OsString::from("APPDATA"), OsString::from(r"C:\different"));
        assert_eq!(
            candidate_paths(&environment, deadline())
                .unwrap_err()
                .reason,
            "ambiguous_environment"
        );
    }

    #[cfg(feature = "test-fixture")]
    #[test]
    fn only_the_existing_exact_fixture_classifier_skips_upstream_configuration() {
        for command in ["juiced-test-worker", "juiced-test-worker.exe"] {
            let inputs = ConfigurationInputs::for_profile(
                Provider::LlamaCpp,
                command,
                &BTreeMap::new(),
                deadline(),
            )
            .unwrap();
            assert!(inputs.summary().is_empty());
        }
        assert!(
            ConfigurationInputs::for_profile(
                Provider::LlamaCpp,
                "production-renamed-worker",
                &BTreeMap::new(),
                deadline()
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaf_and_parent_are_refused_without_following_or_mutation() {
        use std::os::unix::fs::symlink;
        let (_temporary, root) = fixture();
        fs::create_dir(root.join("external")).unwrap();
        fs::write(root.join("external/config"), b"private-external-config").unwrap();
        symlink(root.join("external"), root.join("bound/parent-link")).unwrap();
        symlink(root.join("external/config"), root.join("bound/leaf-link")).unwrap();
        let before = tree(&root);
        for path in [
            root.join("bound/parent-link/config"),
            root.join("bound/leaf-link"),
        ] {
            assert!(ConfigurationInputs::inspect(vec![(Role::User, path)], deadline()).is_err());
        }
        assert_eq!(tree(&root), before);
    }

    #[cfg(unix)]
    #[test]
    fn replaced_ancestor_is_refused_without_touching_the_new_root() {
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        fs::write(&path, b"").unwrap();
        let inputs = inspect(&path);
        fs::rename(root.join("bound"), root.join("retired")).unwrap();
        fs::create_dir(root.join("bound")).unwrap();
        fs::write(&path, b"external-new-root-private-input").unwrap();
        let before = tree(&root);
        assert!(inputs.verify_fresh(deadline()).is_err());
        assert_eq!(tree(&root), before);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_socket_candidates_refuse_without_blocking() {
        use std::os::unix::{ffi::OsStrExt, net::UnixListener};
        let (_temporary, root) = fixture();
        let fifo = root.join("bound/fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let socket = root.join("bound/socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        let before = tree(&root);
        for path in [fifo, socket] {
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = send.send(ConfigurationInputs::inspect(
                    vec![(Role::User, path)],
                    deadline(),
                ));
            });
            assert!(
                receive
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
        }
        assert_eq!(tree(&root), before);
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_existing_parent_and_empty_file_are_unknown_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (_temporary, root) = fixture();
        let path = root.join("bound/config.ini");
        fs::write(&path, b"").unwrap();
        let before = tree(&root);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let file = ConfigurationInputs::inspect(vec![(Role::User, path.clone())], deadline());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(file.is_err());
        fs::set_permissions(root.join("bound"), fs::Permissions::from_mode(0o0)).unwrap();
        let parent = ConfigurationInputs::inspect(vec![(Role::User, path)], deadline());
        fs::set_permissions(root.join("bound"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(parent.is_err());
        assert_eq!(tree(&root), before);
    }
}
