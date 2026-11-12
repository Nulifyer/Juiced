use serde::Serialize;
use std::{fmt, path::Path, time::Instant};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub maps_bytes: usize,
    pub mappings: usize,
    pub files: usize,
    pub file_bytes: u64,
    pub total_bytes: u64,
    pub path_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            maps_bytes: 1024 * 1024,
            mappings: 16_384,
            files: 256,
            file_bytes: 256 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            path_bytes: 4096,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<(), Refusal> {
        if !(1..=4 * 1024 * 1024).contains(&self.maps_bytes)
            || !(1..=65_536).contains(&self.mappings)
            || !(1..=512).contains(&self.files)
            || !(1..=2 * 1024 * 1024 * 1024).contains(&self.file_bytes)
            || !(1..=8 * 1024 * 1024 * 1024).contains(&self.total_bytes)
            || !(1..=65_536).contains(&self.path_bytes)
        {
            return Err(Refusal::unqualified(
                "runtime_identity_limits",
                "Runtime identity limits are zero or exceed the supported inspection bounds.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Unknown,
    Unqualified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub status: Status,
    pub code: &'static str,
    pub message: &'static str,
}
impl Refusal {
    fn unqualified(code: &'static str, message: &'static str) -> Self {
        Self {
            status: Status::Unqualified,
            code,
            message,
        }
    }
    #[cfg(not(target_os = "linux"))]
    fn unsupported() -> Self {
        Self {
            status: Status::Unknown,
            code: "runtime_identity_unsupported",
            message: "Native runtime module observation is unavailable on this platform.",
        }
    }
}
impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Refusal {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessGeneration {
    pub pid: u32,
    pub start_ticks: u64,
    pub executable_device: u64,
    pub executable_inode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileRole {
    MainExecutable,
    Module,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileObservation {
    pub role: FileRole,
    pub paths_digest: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelMapping {
    Vdso,
    Vsyscall,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KernelProvenance {
    pub source: &'static str,
    pub notes_sha256: String,
    pub version_sha256: String,
    pub release_sha256: String,
    pub special_mappings: Vec<KernelMapping>,
}

#[derive(Clone, Serialize)]
pub struct Observation {
    pub source: &'static str,
    pub qualification: &'static str,
    pub device_identity_source: &'static str,
    pub process_generation: ProcessGeneration,
    pub closure_digest: String,
    pub files: Vec<FileObservation>,
    pub kernel: KernelProvenance,
    #[cfg(target_os = "linux")]
    #[serde(skip)]
    manifest: linux::Manifest,
}
impl fmt::Debug for Observation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Observation")
            .field("source", &self.source)
            .field("qualification", &self.qualification)
            .field("process_generation", &self.process_generation)
            .field("closure_digest", &self.closure_digest)
            .field("files", &self.files)
            .field("kernel", &self.kernel)
            .finish()
    }
}

pub fn observe(
    pid: u32,
    expected_executable: &Path,
    deadline: Instant,
    limits: Limits,
) -> Result<Observation, Refusal> {
    #[cfg(target_os = "linux")]
    {
        linux::observe(pid, expected_executable, deadline, limits)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, expected_executable, deadline, limits);
        Err(Refusal::unsupported())
    }
}

pub fn verify_generation(observation: &Observation, deadline: Instant) -> Result<(), Refusal> {
    #[cfg(target_os = "linux")]
    {
        linux::verify_generation(observation, deadline)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (observation, deadline);
        Err(Refusal::unsupported())
    }
}

pub fn verify_bundle_files(
    observation: &Observation,
    root: &Path,
    expected: &std::collections::BTreeMap<std::path::PathBuf, String>,
) -> Result<(), Refusal> {
    #[cfg(target_os = "linux")]
    {
        linux::verify_bundle_files(observation, root, expected)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (observation, root, expected);
        Err(Refusal::unsupported())
    }
}

pub fn verify_loopback_listener(
    observation: &Observation,
    port: u16,
    deadline: Instant,
) -> Result<(), Refusal> {
    #[cfg(target_os = "linux")]
    {
        linux::verify_loopback_listener(observation, port, deadline)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (observation, port, deadline);
        Err(Refusal::unsupported())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::OsString,
        fs::{self, File, Metadata, OpenOptions},
        io::Read,
        os::fd::AsRawFd,
        os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
        },
        path::{Path, PathBuf},
    };

    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct FileKey {
        major: u32,
        minor: u32,
        inode: u64,
    }
    impl FileKey {
        fn metadata(metadata: &Metadata) -> Self {
            Self {
                major: libc::major(metadata.dev()),
                minor: libc::minor(metadata.dev()),
                inode: metadata.ino(),
            }
        }
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Stamp {
        device: u64,
        inode: u64,
        bytes: u64,
        modified: (i64, i64),
        changed: (i64, i64),
        mode: u32,
        links: u64,
    }
    impl Stamp {
        fn metadata(metadata: &Metadata) -> Result<Self, Refusal> {
            if !metadata.is_file() || metadata.nlink() == 0 || metadata.len() == 0 {
                return Err(Refusal::unqualified(
                    "runtime_file_type",
                    "An executable mapping is not a linked nonempty regular file.",
                ));
            }
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
                mode: metadata.mode(),
                links: metadata.nlink(),
            })
        }
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Mapping {
        start: u64,
        end: u64,
        permissions: [u8; 4],
        offset: u64,
        key: FileKey,
        path: Vec<u8>,
    }
    #[derive(Clone, Debug)]
    struct BoundFile {
        paths: Vec<PathBuf>,
        stamp: Stamp,
        key: FileKey,
    }
    #[derive(Clone)]
    pub(super) struct Manifest {
        mappings: Vec<Mapping>,
        files: Vec<BoundFile>,
        expected_executable: PathBuf,
        limits: Limits,
    }

    pub(super) fn verify_bundle_files(
        observation: &Observation,
        root: &Path,
        expected: &BTreeMap<PathBuf, String>,
    ) -> Result<(), Refusal> {
        let mismatch = || {
            Refusal::unqualified(
                "runtime_bundle_mismatch",
                "The observed engine or a bundled executable module does not match the verified runtime bundle.",
            )
        };
        if observation.manifest.files.len() != observation.files.len() {
            return Err(mismatch());
        }
        let mut main_bound = false;
        for (bound, file) in observation.manifest.files.iter().zip(&observation.files) {
            for path in &bound.paths {
                if path.starts_with(root) && expected.get(path) != Some(&file.sha256) {
                    return Err(mismatch());
                }
                if file.role == FileRole::MainExecutable && expected.get(path) == Some(&file.sha256)
                {
                    main_bound = true;
                }
            }
        }
        if !main_bound {
            return Err(mismatch());
        }
        Ok(())
    }

    fn deadline_check(deadline: Instant) -> Result<(), Refusal> {
        if Instant::now() >= deadline {
            return Err(Refusal::unqualified(
                "runtime_identity_deadline",
                "Runtime closure observation exceeded its inspection deadline.",
            ));
        }
        Ok(())
    }
    fn unavailable() -> Refusal {
        Refusal::unqualified(
            "runtime_identity_unavailable",
            "The owned worker's runtime identity could not be inspected.",
        )
    }
    fn changed() -> Refusal {
        Refusal::unqualified(
            "runtime_identity_changed",
            "The owned worker or one of its executable mappings changed during inspection.",
        )
    }
    fn malformed() -> Refusal {
        Refusal::unqualified(
            "runtime_maps_invalid",
            "The worker executable mapping inventory is malformed or truncated.",
        )
    }
    fn bounded_read(path: &Path, maximum: usize, deadline: Instant) -> Result<Vec<u8>, Refusal> {
        deadline_check(deadline)?;
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| unavailable())?;
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            deadline_check(deadline)?;
            let count = input.read(&mut buffer).map_err(|_| unavailable())?;
            if count == 0 {
                break;
            }
            if count > maximum.saturating_sub(output.len()) {
                return Err(Refusal::unqualified(
                    "runtime_identity_limit",
                    "A runtime identity observation exceeded its byte limit.",
                ));
            }
            output.extend_from_slice(&buffer[..count]);
        }
        deadline_check(deadline)?;
        Ok(output)
    }
    fn unsigned(bytes: &[u8], radix: u32) -> Result<u64, Refusal> {
        let text = std::str::from_utf8(bytes).map_err(|_| malformed())?;
        if text.is_empty() || text.starts_with(['+', '-']) {
            return Err(malformed());
        }
        u64::from_str_radix(text, radix).map_err(|_| malformed())
    }
    fn token<'a>(line: &'a [u8], position: &mut usize) -> Result<&'a [u8], Refusal> {
        while line.get(*position).is_some_and(u8::is_ascii_whitespace) {
            *position += 1;
        }
        let start = *position;
        while line
            .get(*position)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            *position += 1;
        }
        if start == *position {
            return Err(malformed());
        }
        Ok(&line[start..*position])
    }
    fn split(bytes: &[u8], separator: u8) -> Result<(&[u8], &[u8]), Refusal> {
        let position = bytes
            .iter()
            .position(|byte| *byte == separator)
            .ok_or_else(malformed)?;
        Ok((&bytes[..position], &bytes[position + 1..]))
    }
    fn parse_maps(
        bytes: &[u8],
        limits: Limits,
        deadline: Instant,
    ) -> Result<Vec<Mapping>, Refusal> {
        if bytes.is_empty() || bytes.len() > limits.maps_bytes || !bytes.ends_with(b"\n") {
            return Err(malformed());
        }
        let mut mappings = Vec::new();
        let mut previous_end = 0;
        for (index, line) in bytes[..bytes.len() - 1]
            .split(|byte| *byte == b'\n')
            .enumerate()
        {
            deadline_check(deadline)?;
            if index >= limits.mappings {
                return Err(Refusal::unqualified(
                    "runtime_identity_limit",
                    "The worker mapping inventory exceeds its entry limit.",
                ));
            }
            let mut position = 0;
            let (start, end) = split(token(line, &mut position)?, b'-')?;
            let start = unsigned(start, 16)?;
            let end = unsigned(end, 16)?;
            if start >= end || start < previous_end {
                return Err(malformed());
            }
            previous_end = end;
            let permissions: [u8; 4] = token(line, &mut position)?
                .try_into()
                .map_err(|_| malformed())?;
            if !matches!(permissions[0], b'r' | b'-')
                || !matches!(permissions[1], b'w' | b'-')
                || !matches!(permissions[2], b'x' | b'-')
                || !matches!(permissions[3], b'p' | b's')
            {
                return Err(malformed());
            }
            let offset = unsigned(token(line, &mut position)?, 16)?;
            let (major, minor) = split(token(line, &mut position)?, b':')?;
            let major = u32::try_from(unsigned(major, 16)?).map_err(|_| malformed())?;
            let minor = u32::try_from(unsigned(minor, 16)?).map_err(|_| malformed())?;
            let inode = unsigned(token(line, &mut position)?, 10)?;
            while line
                .get(position)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            {
                position += 1;
            }
            let path = &line[position..];
            if path.len() > limits.path_bytes || path.contains(&0) {
                return Err(Refusal::unqualified(
                    "runtime_identity_limit",
                    "A runtime module path is invalid or exceeds its byte limit.",
                ));
            }
            if permissions[2] != b'x' {
                continue;
            }
            if permissions[1] == b'w' {
                return Err(Refusal::unqualified(
                    "runtime_writable_executable",
                    "A writable executable mapping has no qualified runtime file identity.",
                ));
            }
            if path.ends_with(b" (deleted)") {
                return Err(Refusal::unqualified(
                    "runtime_deleted_executable",
                    "A deleted or ambiguously named executable mapping cannot be verified.",
                ));
            }
            let kernel = matches!(path, b"[vdso]" | b"[vsyscall]");
            if kernel {
                if inode != 0
                    || major != 0
                    || minor != 0
                    || offset != 0
                    || end - start > 65536
                    || permissions[3] != b'p'
                {
                    return Err(malformed());
                }
            } else if path.first() != Some(&b'/') || inode == 0 {
                return Err(Refusal::unqualified(
                    "runtime_anonymous_executable",
                    "An unknown or anonymous executable mapping has no qualified runtime file identity.",
                ));
            }
            mappings.push(Mapping {
                start,
                end,
                permissions,
                offset,
                key: FileKey {
                    major,
                    minor,
                    inode,
                },
                path: path.to_vec(),
            });
        }
        if mappings.is_empty() {
            return Err(Refusal::unqualified(
                "runtime_no_executable_mappings",
                "The worker has no verifiable executable mapping inventory.",
            ));
        }
        Ok(mappings)
    }
    fn parse_start_ticks(bytes: &[u8], pid: u32) -> Result<u64, Refusal> {
        let start = bytes
            .iter()
            .position(|byte| *byte == b'(')
            .ok_or_else(unavailable)?;
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b')')
            .filter(|end| *end > start)
            .ok_or_else(unavailable)?;
        let claimed_pid = unsigned(bytes[..start].trim_ascii(), 10).map_err(|_| unavailable())?;
        if claimed_pid != u64::from(pid) {
            return Err(changed());
        }
        let fields: Vec<_> = bytes[end + 1..]
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() < 20 || matches!(fields[0], b"Z" | b"X" | b"x") {
            return Err(unavailable());
        }
        unsigned(fields[19], 10).map_err(|_| unavailable())
    }
    fn namespaces(pid: u32) -> Result<(), Refusal> {
        let own = fs::metadata("/proc/self/ns/mnt").map_err(|_| unavailable())?;
        let child = fs::metadata(format!("/proc/{pid}/ns/mnt")).map_err(|_| unavailable())?;
        let root = fs::metadata("/").map_err(|_| unavailable())?;
        let child_root = fs::metadata(format!("/proc/{pid}/root")).map_err(|_| unavailable())?;
        if own.dev() != child.dev()
            || own.ino() != child.ino()
            || root.dev() != child_root.dev()
            || root.ino() != child_root.ino()
        {
            return Err(Refusal::unqualified(
                "runtime_namespace_unqualified",
                "Runtime module identity across a different mount namespace or filesystem root is unqualified.",
            ));
        }
        Ok(())
    }
    fn generation(
        pid: u32,
        expected: &Path,
        deadline: Instant,
    ) -> Result<ProcessGeneration, Refusal> {
        deadline_check(deadline)?;
        if pid == 0 {
            return Err(unavailable());
        }
        let start_ticks = parse_start_ticks(
            &bounded_read(Path::new(&format!("/proc/{pid}/stat")), 64 * 1024, deadline)?,
            pid,
        )?;
        namespaces(pid)?;
        let actual = fs::metadata(format!("/proc/{pid}/exe")).map_err(|_| unavailable())?;
        let configured = fs::metadata(expected).map_err(|_| unavailable())?;
        Stamp::metadata(&actual)?;
        Stamp::metadata(&configured)?;
        if actual.dev() != configured.dev() || actual.ino() != configured.ino() {
            return Err(Refusal::unqualified(
                "runtime_executable_mismatch",
                "The owned process is not executing the expected engine executable.",
            ));
        }
        deadline_check(deadline)?;
        Ok(ProcessGeneration {
            pid,
            start_ticks,
            executable_device: actual.dev(),
            executable_inode: actual.ino(),
        })
    }
    fn maps(pid: u32, limits: Limits, deadline: Instant) -> Result<Vec<Mapping>, Refusal> {
        parse_maps(
            &bounded_read(
                Path::new(&format!("/proc/{pid}/maps")),
                limits.maps_bytes,
                deadline,
            )?,
            limits,
            deadline,
        )
    }
    fn candidates(path: &[u8]) -> Vec<PathBuf> {
        let mut result = vec![PathBuf::from(OsString::from_vec(path.to_vec()))];
        let mut decoded = Vec::with_capacity(path.len());
        let mut position = 0;
        // Linux proc maps escapes newlines only; literal backslashes remain ambiguous.
        while position < path.len() {
            if path[position..].starts_with(b"\\012") {
                decoded.push(b'\n');
                position += 4;
            } else {
                decoded.push(path[position]);
                position += 1;
            }
        }
        if decoded != path {
            result.push(PathBuf::from(OsString::from_vec(decoded)));
        }
        result
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct MountFact {
        major: u32,
        minor: u32,
        btrfs: bool,
    }
    fn parse_mountinfo(
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<BTreeMap<u64, MountFact>, Refusal> {
        if bytes.len() > 1024 * 1024 || !bytes.ends_with(b"\n") {
            return Err(malformed());
        }
        let mut mounts = BTreeMap::new();
        for (index, line) in bytes[..bytes.len() - 1]
            .split(|byte| *byte == b'\n')
            .enumerate()
        {
            deadline_check(deadline)?;
            if index >= 16384 {
                return Err(Refusal::unqualified(
                    "runtime_identity_limit",
                    "The mount identity inventory exceeds its entry limit.",
                ));
            }
            let fields: Vec<_> = line
                .split(u8::is_ascii_whitespace)
                .filter(|field| !field.is_empty())
                .collect();
            if fields.len() < 10 {
                return Err(malformed());
            }
            let id = unsigned(fields[0], 10)?;
            let (major, minor) = split(fields[2], b':')?;
            let separator = fields
                .iter()
                .position(|field| *field == b"-")
                .filter(|position| *position >= 6 && *position + 3 < fields.len())
                .ok_or_else(malformed)?;
            let fact = MountFact {
                major: u32::try_from(unsigned(major, 10)?).map_err(|_| malformed())?,
                minor: u32::try_from(unsigned(minor, 10)?).map_err(|_| malformed())?,
                btrfs: fields[separator + 1] == b"btrfs",
            };
            if mounts.insert(id, fact).is_some() {
                return Err(malformed());
            }
        }
        Ok(mounts)
    }
    #[derive(Default)]
    struct DeviceResolver {
        mounts: Option<BTreeMap<u64, MountFact>>,
        btrfs_used: bool,
    }
    impl DeviceResolver {
        fn matches(
            &mut self,
            file: &File,
            metadata: &Metadata,
            key: &FileKey,
            deadline: Instant,
        ) -> Result<bool, Refusal> {
            if metadata.ino() != key.inode {
                return Ok(false);
            }
            if FileKey::metadata(metadata) == *key {
                return Ok(true);
            }
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
                return Err(unavailable());
            }
            let filesystem = unsafe { filesystem.assume_init() };
            if filesystem.f_type as u32 != libc::BTRFS_SUPER_MAGIC as u32 {
                return Ok(false);
            }
            let mut identity = std::mem::MaybeUninit::<libc::statx>::uninit();
            if unsafe {
                libc::statx(
                    file.as_raw_fd(),
                    c"".as_ptr(),
                    libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                    libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
                    identity.as_mut_ptr(),
                )
            } != 0
            {
                return Err(Refusal::unqualified(
                    "runtime_mount_identity_unknown",
                    "The filesystem requires an unavailable descriptor-bound mount identity.",
                ));
            }
            let identity = unsafe { identity.assume_init() };
            if identity.stx_mask & libc::STATX_MNT_ID == 0
                || identity.stx_ino != metadata.ino()
                || identity.stx_dev_major != libc::major(metadata.dev())
                || identity.stx_dev_minor != libc::minor(metadata.dev())
            {
                return Err(changed());
            }
            if self.mounts.is_none() {
                self.mounts = Some(parse_mountinfo(
                    &bounded_read(Path::new("/proc/self/mountinfo"), 1024 * 1024, deadline)?,
                    deadline,
                )?);
            }
            let Some(mount) = self
                .mounts
                .as_ref()
                .and_then(|mounts| mounts.get(&identity.stx_mnt_id))
            else {
                return Err(changed());
            };
            // Btrfs stat devices identify subvolumes; proc maps uses the superblock device.
            if !mount.btrfs || mount.major != key.major || mount.minor != key.minor {
                return Ok(false);
            }
            self.btrfs_used = true;
            Ok(true)
        }
    }
    #[cfg(test)]
    fn open_bound(mapping: &Mapping, deadline: Instant) -> Result<(File, PathBuf, Stamp), Refusal> {
        open_resolved(mapping, deadline, &mut DeviceResolver::default())
    }
    fn open_resolved(
        mapping: &Mapping,
        deadline: Instant,
        resolver: &mut DeviceResolver,
    ) -> Result<(File, PathBuf, Stamp), Refusal> {
        let mut matched = None;
        for path in candidates(&mapping.path) {
            deadline_check(deadline)?;
            let Ok(file) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(&path)
            else {
                continue;
            };
            let metadata = file.metadata().map_err(|_| unavailable())?;
            if !metadata.is_file() || !resolver.matches(&file, &metadata, &mapping.key, deadline)? {
                continue;
            }
            let stamp = Stamp::metadata(&metadata)?;
            if mapping.offset >= stamp.bytes {
                return Err(Refusal::unqualified(
                    "runtime_mapped_file_mismatch",
                    "An executable mapping starts outside its observed file bytes.",
                ));
            }
            let canonical = fs::canonicalize(&path).map_err(|_| unavailable())?;
            if Stamp::metadata(&fs::metadata(&canonical).map_err(|_| unavailable())?)? != stamp {
                return Err(changed());
            }
            if matched
                .as_ref()
                .is_some_and(|(_, previous, _)| previous != &canonical)
            {
                return Err(Refusal::unqualified(
                    "runtime_path_ambiguous",
                    "The mapped executable pathname is ambiguous despite inode validation.",
                ));
            }
            matched = Some((file, canonical, stamp));
        }
        matched.ok_or_else(|| Refusal::unqualified("runtime_mapped_file_mismatch", "An executable mapping does not match a readable linked regular file at its observed path."))
    }
    fn validate_paths(file: &BoundFile, deadline: Instant) -> Result<(), Refusal> {
        for path in &file.paths {
            deadline_check(deadline)?;
            let current = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(path)
                .map_err(|_| changed())?;
            let metadata = current.metadata().map_err(|_| changed())?;
            if FileKey::metadata(&metadata) != file.key || Stamp::metadata(&metadata)? != file.stamp
            {
                return Err(changed());
            }
        }
        Ok(())
    }
    fn hash_file(
        file: &mut File,
        stamp: &Stamp,
        remaining: &mut u64,
        limits: Limits,
        deadline: Instant,
    ) -> Result<String, Refusal> {
        if stamp.bytes > limits.file_bytes || stamp.bytes > *remaining {
            return Err(Refusal::unqualified(
                "runtime_identity_limit",
                "The runtime executable file closure exceeds its file or aggregate byte limit.",
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        let mut bytes = 0_u64;
        while bytes < stamp.bytes {
            deadline_check(deadline)?;
            let wanted = (stamp.bytes - bytes).min(buffer.len() as u64) as usize;
            let count = file
                .read(&mut buffer[..wanted])
                .map_err(|_| unavailable())?;
            if count == 0 {
                return Err(changed());
            }
            bytes += count as u64;
            hash.update(&buffer[..count]);
        }
        if bytes != stamp.bytes
            || Stamp::metadata(&file.metadata().map_err(|_| changed())?)? != *stamp
        {
            return Err(changed());
        }
        *remaining -= bytes;
        deadline_check(deadline)?;
        Ok(format!("{:x}", hash.finalize()))
    }
    fn kernel(mappings: &[Mapping], deadline: Instant) -> Result<KernelProvenance, Refusal> {
        let mut special_mappings = Vec::new();
        for mapping in mappings {
            let kind = match mapping.path.as_slice() {
                b"[vdso]" => KernelMapping::Vdso,
                b"[vsyscall]" => KernelMapping::Vsyscall,
                _ => continue,
            };
            if special_mappings.contains(&kind) {
                return Err(malformed());
            }
            special_mappings.push(kind);
        }
        special_mappings.sort();
        let digest = |path: &str, limit| -> Result<String, Refusal> {
            let value = bounded_read(Path::new(path), limit, deadline)?;
            if value.is_empty() {
                return Err(Refusal::unqualified(
                    "runtime_kernel_unknown",
                    "The kernel provenance for special executable mappings is unavailable.",
                ));
            }
            Ok(format!("{:x}", Sha256::digest(value)))
        };
        Ok(KernelProvenance {
            source: "linux-kernel-provenance",
            notes_sha256: digest("/sys/kernel/notes", 65536)?,
            version_sha256: digest("/proc/version", 16384)?,
            release_sha256: digest("/proc/sys/kernel/osrelease", 4096)?,
            special_mappings,
        })
    }
    fn digest_part(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    fn paths_digest(paths: &[PathBuf]) -> String {
        let mut hash = Sha256::new();
        for path in paths {
            digest_part(&mut hash, path.as_os_str().as_bytes());
        }
        format!("{:x}", hash.finalize())
    }
    fn closure_digest(files: &[FileObservation], kernel: &KernelProvenance) -> String {
        let mut sorted: Vec<_> = files.iter().collect();
        sorted.sort_by(|left, right| left.paths_digest.cmp(&right.paths_digest));
        let mut hash = Sha256::new();
        digest_part(&mut hash, b"juiced-linux-runtime-file-observation-v1");
        for file in sorted {
            digest_part(
                &mut hash,
                match file.role {
                    FileRole::MainExecutable => b"main",
                    FileRole::Module => b"module",
                },
            );
            digest_part(&mut hash, file.paths_digest.as_bytes());
            digest_part(&mut hash, file.sha256.as_bytes());
            hash.update(file.bytes.to_le_bytes());
        }
        for value in [
            &kernel.notes_sha256,
            &kernel.version_sha256,
            &kernel.release_sha256,
        ] {
            digest_part(&mut hash, value.as_bytes());
        }
        for special in &kernel.special_mappings {
            digest_part(
                &mut hash,
                match special {
                    KernelMapping::Vdso => b"vdso",
                    KernelMapping::Vsyscall => b"vsyscall",
                },
            );
        }
        format!("{:x}", hash.finalize())
    }
    fn validate_generation(
        before: &ProcessGeneration,
        after: &ProcessGeneration,
    ) -> Result<(), Refusal> {
        if before != after {
            Err(changed())
        } else {
            Ok(())
        }
    }

    pub(super) fn observe(
        pid: u32,
        expected_executable: &Path,
        deadline: Instant,
        limits: Limits,
    ) -> Result<Observation, Refusal> {
        limits.validate()?;
        deadline_check(deadline)?;
        let expected_executable =
            fs::canonicalize(expected_executable).map_err(|_| unavailable())?;
        let process_generation = generation(pid, &expected_executable, deadline)?;
        let mappings = maps(pid, limits, deadline)?;
        let kernel = kernel(&mappings, deadline)?;
        let mut resolver = DeviceResolver::default();
        let mut remaining = limits.total_bytes;
        let mut grouped: BTreeMap<(u64, u64), (String, BoundFile)> = BTreeMap::new();
        for mapping in &mappings {
            if mapping.path.first() != Some(&b'/') {
                continue;
            }
            deadline_check(deadline)?;
            let (mut file, path, stamp) = open_resolved(mapping, deadline, &mut resolver)?;
            let identity = (stamp.device, stamp.inode);
            if let Some((_, bound)) = grouped.get_mut(&identity) {
                if bound.stamp != stamp {
                    return Err(changed());
                }
                if !bound.paths.contains(&path) {
                    bound.paths.push(path);
                }
                continue;
            }
            if grouped.len() >= limits.files {
                return Err(Refusal::unqualified(
                    "runtime_identity_limit",
                    "The executable runtime closure exceeds its file count limit.",
                ));
            }
            let key = FileKey {
                major: libc::major(stamp.device),
                minor: libc::minor(stamp.device),
                inode: stamp.inode,
            };
            let sha256 = hash_file(&mut file, &stamp, &mut remaining, limits, deadline)?;
            grouped.insert(
                identity,
                (
                    sha256,
                    BoundFile {
                        paths: vec![path],
                        stamp,
                        key,
                    },
                ),
            );
        }
        let main = (
            process_generation.executable_device,
            process_generation.executable_inode,
        );
        if !grouped.contains_key(&main) {
            return Err(Refusal::unqualified(
                "runtime_executable_unmapped",
                "The expected engine executable is absent from the executable mapping inventory.",
            ));
        }
        let mut files = Vec::with_capacity(grouped.len());
        let mut manifest_files = Vec::with_capacity(grouped.len());
        for (identity, (sha256, mut bound)) in grouped {
            bound.paths.sort();
            files.push(FileObservation {
                role: if identity == main {
                    FileRole::MainExecutable
                } else {
                    FileRole::Module
                },
                paths_digest: paths_digest(&bound.paths),
                sha256,
                bytes: bound.stamp.bytes,
            });
            manifest_files.push(bound);
        }
        for file in &manifest_files {
            validate_paths(file, deadline)?;
        }
        if maps(pid, limits, deadline)? != mappings || self::kernel(&mappings, deadline)? != kernel
        {
            return Err(changed());
        }
        validate_generation(
            &process_generation,
            &generation(pid, &expected_executable, deadline)?,
        )?;
        let closure_digest = closure_digest(&files, &kernel);
        Ok(Observation {
            source: "linux-procfs",
            qualification: "runtime-file-observation-only",
            device_identity_source: if resolver.btrfs_used {
                "fstat-dev-inode+statx-btrfs-mount"
            } else {
                "fstat-dev-inode"
            },
            process_generation,
            closure_digest,
            files,
            kernel,
            manifest: Manifest {
                mappings,
                files: manifest_files,
                expected_executable,
                limits,
            },
        })
    }
    pub(super) fn verify_generation(
        observation: &Observation,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        deadline_check(deadline)?;
        let manifest = &observation.manifest;
        let pid = observation.process_generation.pid;
        validate_generation(
            &observation.process_generation,
            &generation(pid, &manifest.expected_executable, deadline)?,
        )?;
        if maps(pid, manifest.limits, deadline)? != manifest.mappings {
            return Err(changed());
        }
        for file in &manifest.files {
            validate_paths(file, deadline)?;
        }
        if kernel(&manifest.mappings, deadline)? != observation.kernel
            || maps(pid, manifest.limits, deadline)? != manifest.mappings
        {
            return Err(changed());
        }
        let fresh = observe(
            pid,
            &manifest.expected_executable,
            deadline,
            manifest.limits,
        )?;
        validate_generation(&observation.process_generation, &fresh.process_generation)?;
        if fresh.closure_digest != observation.closure_digest
            || fresh.manifest.mappings != manifest.mappings
        {
            return Err(changed());
        }
        for file in &manifest.files {
            validate_paths(file, deadline)?;
        }
        deadline_check(deadline)
    }

    fn owned_sockets(pid: u32, deadline: Instant) -> Result<BTreeSet<u64>, Refusal> {
        let entries = fs::read_dir(format!("/proc/{pid}/fd")).map_err(|_| unavailable())?;
        let mut sockets = BTreeSet::new();
        for (index, entry) in entries.enumerate() {
            deadline_check(deadline)?;
            if index >= 4096 {
                return Err(Refusal::unqualified(
                    "runtime_listener_limit",
                    "The worker file descriptor inventory exceeds its inspection limit.",
                ));
            }
            let entry = entry.map_err(|_| unavailable())?;
            let target = match fs::read_link(entry.path()) {
                Ok(target) => target,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(unavailable()),
            };
            let bytes = target.as_os_str().as_bytes();
            if let Some(inode) = bytes
                .strip_prefix(b"socket:[")
                .and_then(|bytes| bytes.strip_suffix(b"]"))
            {
                let inode = unsigned(inode, 10)?;
                if inode == 0 {
                    return Err(malformed());
                }
                sockets.insert(inode);
            }
        }
        Ok(sockets)
    }
    fn network_namespace(pid: u32) -> Result<(), Refusal> {
        let own = fs::metadata("/proc/self/ns/net").map_err(|_| unavailable())?;
        let child = fs::metadata(format!("/proc/{pid}/ns/net")).map_err(|_| unavailable())?;
        if own.dev() != child.dev() || own.ino() != child.ino() {
            return Err(Refusal::unqualified(
                "runtime_network_namespace",
                "The owned worker is in a different network namespace from the loopback HTTP client.",
            ));
        }
        Ok(())
    }
    fn loopback_address(bytes: &[u8], ipv6: bool) -> Result<bool, Refusal> {
        if bytes.len() != if ipv6 { 32 } else { 8 } {
            return Err(malformed());
        }
        let mut address = Vec::with_capacity(if ipv6 { 16 } else { 4 });
        for word in bytes.chunks_exact(8) {
            let word = u32::try_from(unsigned(word, 16)?).map_err(|_| malformed())?;
            address.extend_from_slice(&word.to_ne_bytes());
        }
        Ok(if ipv6 {
            address.as_slice() == std::net::Ipv6Addr::LOCALHOST.octets()
        } else {
            address == [127, 0, 0, 1]
        })
    }
    fn listener_table(
        bytes: &[u8],
        port: u16,
        ipv6: bool,
        sockets: &BTreeSet<u64>,
        deadline: Instant,
    ) -> Result<BTreeSet<u64>, Refusal> {
        if bytes.len() > 2 * 1024 * 1024 || !bytes.ends_with(b"\n") {
            return Err(malformed());
        }
        let mut rows = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
        let header = rows.next().ok_or_else(malformed)?;
        if !header
            .windows(b"local_address".len())
            .any(|value| value == b"local_address")
            || !header
                .windows(b"inode".len())
                .any(|value| value == b"inode")
        {
            return Err(malformed());
        }
        let mut listeners = BTreeSet::new();
        for (index, row) in rows.enumerate() {
            deadline_check(deadline)?;
            if index >= 16384 {
                return Err(Refusal::unqualified(
                    "runtime_listener_limit",
                    "The TCP listener inventory exceeds its inspection limit.",
                ));
            }
            let fields: Vec<_> = row
                .split(u8::is_ascii_whitespace)
                .filter(|field| !field.is_empty())
                .collect();
            if fields.len() < 10 {
                return Err(malformed());
            }
            if unsigned(fields[3], 16)? != 0x0a {
                continue;
            }
            let (address, observed_port) = split(fields[1], b':')?;
            let observed_port =
                u16::try_from(unsigned(observed_port, 16)?).map_err(|_| malformed())?;
            if observed_port != port {
                continue;
            }
            if !loopback_address(address, ipv6)? {
                return Err(Refusal::unqualified(
                    "runtime_listener_not_loopback",
                    "The selected port has a listener outside the exact loopback address.",
                ));
            }
            let inode = unsigned(fields[9], 10)?;
            if !sockets.contains(&inode) {
                return Err(Refusal::unqualified(
                    "runtime_listener_other_owner",
                    "The selected loopback port is not exclusively held by the owned worker.",
                ));
            }
            if !ipv6 {
                listeners.insert(inode);
            }
        }
        Ok(listeners)
    }
    fn listeners(pid: u32, port: u16, deadline: Instant) -> Result<BTreeSet<u64>, Refusal> {
        let sockets = owned_sockets(pid, deadline)?;
        let ipv4 = bounded_read(
            Path::new(&format!("/proc/{pid}/net/tcp")),
            2 * 1024 * 1024,
            deadline,
        )?;
        let ipv6 = bounded_read(
            Path::new(&format!("/proc/{pid}/net/tcp6")),
            2 * 1024 * 1024,
            deadline,
        )?;
        let listeners = listener_table(&ipv4, port, false, &sockets, deadline)?;
        listener_table(&ipv6, port, true, &sockets, deadline)?;
        if listeners.is_empty() {
            return Err(Refusal::unqualified(
                "runtime_listener_unverified",
                "The owned worker has no verified IPv4 loopback listener on the selected port.",
            ));
        }
        Ok(listeners)
    }
    pub(super) fn verify_loopback_listener(
        observation: &Observation,
        port: u16,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        if port == 0 {
            return Err(Refusal::unqualified(
                "runtime_listener_invalid",
                "A nonzero loopback listener port is required.",
            ));
        }
        deadline_check(deadline)?;
        let pid = observation.process_generation.pid;
        let expected = &observation.manifest.expected_executable;
        validate_generation(
            &observation.process_generation,
            &generation(pid, expected, deadline)?,
        )?;
        network_namespace(pid)?;
        let before = listeners(pid, port, deadline)?;
        if listeners(pid, port, deadline)? != before {
            return Err(changed());
        }
        network_namespace(pid)?;
        validate_generation(
            &observation.process_generation,
            &generation(pid, expected, deadline)?,
        )?;
        deadline_check(deadline)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Child, Command, Stdio},
            time::Duration,
        };
        fn deadline() -> Instant {
            Instant::now() + Duration::from_secs(10)
        }
        fn parse(input: &[u8]) -> Result<Vec<Mapping>, Refusal> {
            parse_maps(input, Limits::default(), deadline())
        }
        fn fixture() -> tempfile::TempDir {
            tempfile::Builder::new()
                .prefix(".runtime-identity-")
                .tempdir_in(env!("CARGO_MANIFEST_DIR"))
                .unwrap()
        }
        fn mapping(path: &Path) -> Mapping {
            Mapping {
                start: 0x1000,
                end: 0x2000,
                permissions: *b"r-xp",
                offset: 0,
                key: FileKey::metadata(&fs::metadata(path).unwrap()),
                path: path.as_os_str().as_bytes().to_vec(),
            }
        }
        #[test]
        fn maps_parse_spaces_kernel_provenance_and_native_octal_path_ambiguity() {
            let maps = parse(b"1000-2000 r-xp 0000 08:01 10 /space path/module.so \n2000-3000 r--p 0000 08:01 11 /weights.gguf\n3000-4000 r-xp 0000 00:00 0 [vdso]\nffffffffff600000-ffffffffff601000 --xp 0000 00:00 0 [vsyscall]\n").unwrap();
            assert_eq!(maps.len(), 3);
            assert_eq!(maps[0].path, b"/space path/module.so ");
            assert_eq!(maps[1].path, b"[vdso]");
            assert_eq!(candidates(b"/literal\\040space.so").len(), 1);
            assert_eq!(
                candidates(b"/line\\012name.so")[1].as_os_str().as_bytes(),
                b"/line\nname.so"
            );
            let dir = fixture();
            let newline = dir.path().join("line\nname.so");
            let literal = dir.path().join("line\\012name.so");
            fs::write(&newline, b"newline").unwrap();
            fs::write(&literal, b"literal").unwrap();
            let mut entry = mapping(&newline);
            entry.path = literal.as_os_str().as_bytes().to_vec();
            assert_eq!(open_bound(&entry, deadline()).unwrap().1, newline);
            assert_eq!(
                open_bound(&mapping(&literal), deadline()).unwrap().1,
                literal
            );
        }
        #[test]
        fn deleted_unknown_anonymous_writable_and_malformed_mappings_refuse() {
            for input in [
                b"1000-2000 r-xp 0000 08:01 1 /lib/a.so (deleted)\n".as_slice(),
                b"1000-2000 rwxp 0000 08:01 1 /lib/a.so\n",
                b"1000-2000 r-xp 0000 00:00 0\n",
                b"1000-2000 r-xp 0000 00:00 0 [anon:jit]\n",
                b"1000-2000 r-xp 0000 00:00 0 [unknown]\n",
                b"1000-2000 r-xp 0000 00:00 0 [vdso]",
                b"1000-1000 r-xp 0000 08:01 1 /lib/a.so\n",
                b"1000-3000 r-xp 0000 08:01 1 /lib/a.so\n2000-4000 r-xp 0000 08:01 2 /lib/b.so\n",
            ] {
                assert_eq!(parse(input).unwrap_err().status, Status::Unqualified);
            }
            assert!(parse(b"1000-2000 r--p 0000 08:01 1 /weights.gguf\n").is_err());
        }
        #[test]
        fn pid_comm_parentheses_reuse_and_executable_generation_are_bound() {
            let mut stat = b"123 (a tricky ) name) S".to_vec();
            for _ in 0..18 {
                stat.extend(b" 0");
            }
            stat.extend(b" 456\n");
            assert_eq!(parse_start_ticks(&stat, 123).unwrap(), 456);
            assert!(parse_start_ticks(&stat, 124).is_err());
            let before = ProcessGeneration {
                pid: 123,
                start_ticks: 456,
                executable_device: 7,
                executable_inode: 8,
            };
            let mut after = before.clone();
            after.start_ticks += 1;
            assert!(validate_generation(&before, &after).is_err());
            let mut after = before.clone();
            after.executable_inode += 1;
            assert!(validate_generation(&before, &after).is_err());
        }
        #[test]
        fn changed_files_symlinks_and_fifo_replacements_cannot_reuse_observation() {
            let directory = fixture();
            let path = directory.path().join("module.so");
            fs::write(&path, b"original").unwrap();
            let entry = mapping(&path);
            let (mut file, canonical, stamp) = open_bound(&entry, deadline()).unwrap();
            let bound = BoundFile {
                paths: vec![canonical],
                stamp: stamp.clone(),
                key: entry.key.clone(),
            };
            let mut budget = 100;
            let digest = hash_file(
                &mut file,
                &stamp,
                &mut budget,
                Limits::default(),
                deadline(),
            )
            .unwrap();
            assert_eq!(digest.len(), 64);
            assert_eq!(budget, 92);
            fs::write(&path, b"changed length").unwrap();
            assert!(validate_paths(&bound, deadline()).is_err());
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(directory.path().join("outside"), &path).unwrap();
            assert!(open_bound(&entry, deadline()).is_err());
            fs::remove_file(&path).unwrap();
            let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            assert!(open_bound(&entry, deadline()).is_err());
        }
        #[test]
        fn limits_and_deadlines_reject_before_claiming_a_runtime_identity() {
            let directory = fixture();
            let path = directory.path().join("module.so");
            fs::write(&path, b"eight123").unwrap();
            let (mut file, _, stamp) = open_bound(&mapping(&path), deadline()).unwrap();
            assert_eq!(
                hash_file(
                    &mut file,
                    &stamp,
                    &mut 100,
                    Limits {
                        file_bytes: 4,
                        ..Limits::default()
                    },
                    deadline()
                )
                .unwrap_err()
                .code,
                "runtime_identity_limit"
            );
            let (mut file, _, stamp) = open_bound(&mapping(&path), deadline()).unwrap();
            assert!(hash_file(&mut file, &stamp, &mut 4, Limits::default(), deadline()).is_err());
            assert!(
                Limits {
                    files: 0,
                    ..Limits::default()
                }
                .validate()
                .is_err()
            );
            assert!(
                parse_maps(
                    b"1000-2000 r-xp 0000 08:01 1 /lib/a.so\n",
                    Limits {
                        mappings: 0,
                        ..Limits::default()
                    },
                    deadline()
                )
                .is_err()
            );
            assert_eq!(
                observe(
                    std::process::id(),
                    &std::env::current_exe().unwrap(),
                    Instant::now(),
                    Limits::default()
                )
                .unwrap_err()
                .code,
                "runtime_identity_deadline"
            );
        }
        #[test]
        fn semantic_digest_changes_with_modules_or_kernel_and_ignores_inventory_order() {
            let kernel = KernelProvenance {
                source: "linux-kernel-provenance",
                notes_sha256: "a".into(),
                version_sha256: "b".into(),
                release_sha256: "c".into(),
                special_mappings: vec![KernelMapping::Vdso],
            };
            let first = FileObservation {
                role: FileRole::MainExecutable,
                paths_digest: "a".into(),
                sha256: "engine".into(),
                bytes: 1,
            };
            let second = FileObservation {
                role: FileRole::Module,
                paths_digest: "b".into(),
                sha256: "math".into(),
                bytes: 2,
            };
            let before = closure_digest(&[first.clone(), second.clone()], &kernel);
            assert_eq!(
                before,
                closure_digest(&[second.clone(), first.clone()], &kernel)
            );
            let mut changed = second;
            changed.sha256 = "new math".into();
            assert_ne!(before, closure_digest(&[first.clone(), changed], &kernel));
            let mut changed_kernel = kernel;
            changed_kernel.notes_sha256 = "different kernel".into();
            assert_ne!(before, closure_digest(&[first], &changed_kernel));
        }
        #[test]
        fn btrfs_mount_device_is_bound_by_mount_id_not_inode_only() {
            let mounts = parse_mountinfo(b"62 44 0:37 /@home /home rw shared:174 - btrfs /dev/mapper/disk rw\n44 1 0:37 /@ / rw - btrfs /dev/mapper/disk rw\n", deadline()).unwrap();
            assert_eq!(
                mounts[&62],
                MountFact {
                    major: 0,
                    minor: 37,
                    btrfs: true
                }
            );
            assert!(parse_mountinfo(b"62 44 0:37 /@home /home rw - btrfs /disk rw\n62 44 0:37 /@ / rw - ext4 /disk rw\n", deadline()).is_err());
            let directory = fixture();
            let path = directory.path().join("module.so");
            fs::write(&path, b"code").unwrap();
            let mut entry = mapping(&path);
            entry.key.inode += 1;
            assert!(open_bound(&entry, deadline()).is_err());
            let mut entry = mapping(&path);
            entry.key.minor = u32::MAX;
            assert!(open_bound(&entry, deadline()).is_err());
        }

        #[tokio::test]
        #[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; observes an owned native engine's actual loaded modules"]
        async fn actual_llama_engine_closure_binds_native_math_libraries_and_kernel_provenance() {
            let engine = PathBuf::from(
                std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("Set JUICED_TEST_LLAMA_SERVER"),
            );
            let model =
                PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("Set JUICED_TEST_GGUF"));
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let mut child = OwnedChild(
                Command::new(&engine)
                    .args([
                        "--host",
                        "127.0.0.1",
                        "--port",
                        &port.to_string(),
                        "--model",
                        model.to_str().unwrap(),
                        "--slots",
                        "--parallel",
                        "1",
                        "--ctx-size",
                        "512",
                        "--threads",
                        "2",
                        "--n-gpu-layers",
                        "0",
                    ])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_millis(500))
                .build()
                .unwrap();
            let ready_deadline = Instant::now() + Duration::from_secs(30);
            loop {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "The owned engine exited before readiness"
                );
                assert!(
                    Instant::now() < ready_deadline,
                    "The owned engine did not become ready"
                );
                if let Ok(response) = client
                    .get(format!("http://127.0.0.1:{port}/health"))
                    .send()
                    .await
                    && response.status().is_success()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let observed = observe(child.0.id(), &engine, deadline(), Limits::default()).unwrap();
            assert!(
                observed
                    .files
                    .iter()
                    .any(|file| file.role == FileRole::Module)
            );
            assert_eq!(observed.kernel.source, "linux-kernel-provenance");
            verify_generation(&observed, deadline()).unwrap();
            verify_loopback_listener(&observed, port, deadline()).unwrap();
            println!(
                "runtime closure files={} digest={} device_source={}",
                observed.files.len(),
                observed.closure_digest,
                observed.device_identity_source
            );
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            assert!(verify_generation(&observed, deadline()).is_err());
            assert!(verify_loopback_listener(&observed, port, deadline()).is_err());
        }

        #[test]
        fn tcp_inventory_rejects_wildcards_unowned_and_cross_family_only_proofs() {
            let header = "  sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n";
            let address = format!("{:08X}", u32::from_ne_bytes([127, 0, 0, 1]));
            let row = |address: &str, inode: u64| {
                format!(
                    "0: {address}:1234 00000000:0000 0A 0000:0000 00:0000 0000 1000 0 {inode}\n"
                )
            };
            let owned = BTreeSet::from([42]);
            let valid = format!("{header}{}", row(&address, 42));
            assert_eq!(
                listener_table(valid.as_bytes(), 0x1234, false, &owned, deadline()).unwrap(),
                owned
            );
            let unowned = format!("{header}{}", row(&address, 43));
            assert_eq!(
                listener_table(unowned.as_bytes(), 0x1234, false, &owned, deadline())
                    .unwrap_err()
                    .code,
                "runtime_listener_other_owner"
            );
            let wildcard = format!("{header}{}", row("00000000", 42));
            assert_eq!(
                listener_table(wildcard.as_bytes(), 0x1234, false, &owned, deadline())
                    .unwrap_err()
                    .code,
                "runtime_listener_not_loopback"
            );
            let ipv6 = std::net::Ipv6Addr::LOCALHOST
                .octets()
                .chunks_exact(4)
                .map(|word| format!("{:08X}", u32::from_ne_bytes(word.try_into().unwrap())))
                .collect::<String>();
            let ipv6 = format!("{header}{}", row(&ipv6, 42));
            assert!(
                listener_table(ipv6.as_bytes(), 0x1234, true, &owned, deadline())
                    .unwrap()
                    .is_empty()
            );
            assert!(listener_table(b"truncated", 0x1234, false, &owned, deadline()).is_err());
        }

        #[test]
        fn runtime_identity_child() {
            if std::env::var_os("JUICED_RUNTIME_IDENTITY_CHILD").is_some() {
                let listener = std::net::TcpListener::bind(
                    if std::env::var_os("JUICED_RUNTIME_IDENTITY_BIND_ALL").is_some() {
                        "0.0.0.0:0"
                    } else {
                        "127.0.0.1:0"
                    },
                )
                .unwrap();
                println!(
                    "runtime identity fixture ready port={}",
                    listener.local_addr().unwrap().port()
                );
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(30));
            }
        }
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        #[test]
        fn actual_subprocess_runtime_closure_is_redacted_and_freshness_fails_after_exit() {
            let executable = std::env::current_exe().unwrap();
            let mut child = OwnedChild(
                Command::new(&executable)
                    .args(["--nocapture", "runtime_identity_child"])
                    .env("JUICED_RUNTIME_IDENTITY_CHILD", "1")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let mut output = BufReader::new(child.0.stdout.take().unwrap());
            let port = loop {
                let mut line = String::new();
                assert!(output.read_line(&mut line).unwrap() > 0);
                if let Some((_, port)) = line.split_once("runtime identity fixture ready port=") {
                    break port.trim().parse::<u16>().unwrap();
                }
            };
            let observed =
                observe(child.0.id(), &executable, deadline(), Limits::default()).unwrap();
            assert!(!observed.files.is_empty());
            assert_eq!(
                observed
                    .files
                    .iter()
                    .filter(|file| file.role == FileRole::MainExecutable)
                    .count(),
                1
            );
            verify_generation(&observed, deadline()).unwrap();
            verify_loopback_listener(&observed, port, deadline()).unwrap();
            let unrelated = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            assert_eq!(
                verify_loopback_listener(
                    &observed,
                    unrelated.local_addr().unwrap().port(),
                    deadline()
                )
                .unwrap_err()
                .code,
                "runtime_listener_other_owner"
            );
            assert!(verify_loopback_listener(&observed, 0, deadline()).is_err());
            let again = observe(child.0.id(), &executable, deadline(), Limits::default()).unwrap();
            assert_eq!(observed.closure_digest, again.closure_digest);
            assert!(
                !serde_json::to_string(&observed)
                    .unwrap()
                    .contains(executable.to_str().unwrap())
            );
            assert!(!format!("{observed:?}").contains(executable.to_str().unwrap()));
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            assert!(verify_generation(&observed, deadline()).is_err());
            assert!(verify_loopback_listener(&observed, port, deadline()).is_err());
        }
    }
}
