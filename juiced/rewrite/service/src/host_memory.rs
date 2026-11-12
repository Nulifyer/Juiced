use serde::Serialize;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    UnsupportedPlatform,
    Deadline,
    ReadLimit,
    ReadUnavailable,
    InvalidMetadata,
    HostSampleUnavailable,
    CgroupUnresolved,
    CgroupConstraintUnknown,
    ProcessLimitsUnqualified,
}

#[derive(Clone, Debug, Serialize)]
pub struct Constraint {
    pub limit_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub source: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub host_total_bytes: Option<u64>,
    pub host_available_bytes: Option<u64>,
    pub source: &'static str,
    pub containment: &'static str,
    pub constraints_complete: bool,
    pub constraints: Vec<Constraint>,
    pub reason: Option<Reason>,
    pub observed_at_unix_ms: u64,
    pub files_read: u32,
    pub bytes_read: u64,
    available_bytes: Option<u64>,
    #[serde(skip)]
    observed_at: Instant,
}

impl Observation {
    pub fn available_bytes(&self) -> Option<u64> {
        self.available_bytes
    }

    pub fn observed_at(&self) -> Instant {
        self.observed_at
    }

    fn unknown(reason: Reason) -> Self {
        Self {
            host_total_bytes: None,
            host_available_bytes: None,
            source: "unavailable",
            containment: "unknown",
            constraints_complete: false,
            constraints: Vec::new(),
            reason: Some(reason),
            observed_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            files_read: 0,
            bytes_read: 0,
            available_bytes: None,
            observed_at: Instant::now(),
        }
    }
}

/// Performs synchronous, bounded reads. Invoke outside admission locks on a blocking worker.
/// The deadline is cooperative and cannot interrupt a kernel filesystem operation.
pub fn sample(deadline: Instant) -> Observation {
    #[cfg(target_os = "linux")]
    {
        linux::sample_at(
            std::path::Path::new("/proc"),
            std::path::Path::new("/"),
            None,
            deadline,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = deadline;
        Observation::unknown(Reason::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        ffi::{CString, OsStr},
        fs::{File, OpenOptions},
        io::{self, Read},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        },
        path::Path,
    };

    const MAXIMUM_FILE_BYTES: usize = 1024 * 1024;
    const MAXIMUM_TOTAL_BYTES: usize = 4 * 1024 * 1024;
    const MAXIMUM_PATH_BYTES: usize = 4096;
    const MAXIMUM_COMPONENTS: usize = 128;
    const MAXIMUM_LINES: usize = 16384;
    const MAXIMUM_TOKEN_BYTES: usize = 256;
    const MAXIMUM_CONTROLLERS_BYTES: usize = 16384;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Failure {
        Missing,
        Unavailable,
        Invalid,
        Limit,
        Deadline,
    }

    impl Failure {
        fn reason(self) -> Reason {
            match self {
                Self::Missing | Self::Unavailable => Reason::ReadUnavailable,
                Self::Invalid => Reason::InvalidMetadata,
                Self::Limit => Reason::ReadLimit,
                Self::Deadline => Reason::Deadline,
            }
        }
    }

    fn io_failure(error: io::Error) -> Failure {
        if error.kind() == io::ErrorKind::NotFound {
            Failure::Missing
        } else {
            Failure::Unavailable
        }
    }

    struct Reader {
        deadline: Instant,
        files_read: u32,
        bytes_read: usize,
    }

    impl Reader {
        fn check(&self) -> Result<(), Failure> {
            if Instant::now() >= self.deadline {
                Err(Failure::Deadline)
            } else {
                Ok(())
            }
        }

        fn root(&self, path: &Path) -> Result<File, Failure> {
            self.check()?;
            OpenOptions::new()
                .read(true)
                .custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
                .open(path)
                .map_err(io_failure)
        }

        fn open(
            &self,
            directory: &File,
            name: &OsStr,
            is_directory: bool,
        ) -> Result<File, Failure> {
            self.check()?;
            if name.as_bytes().is_empty()
                || name.as_bytes().len() > MAXIMUM_PATH_BYTES
                || name.as_bytes().contains(&b'/')
                || name == "."
                || name == ".."
            {
                return Err(Failure::Invalid);
            }
            let name = CString::new(name.as_bytes()).map_err(|_| Failure::Invalid)?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if is_directory { libc::O_DIRECTORY } else { 0 };
            let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io_failure(io::Error::last_os_error()));
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        }

        fn native_process(&mut self, proc_root: &File) -> Result<File, Failure> {
            self.check()?;
            let mut target = [0u8; 32];
            // The procfs mount may expose a different PID namespace than getpid().
            let length = unsafe {
                libc::readlinkat(
                    proc_root.as_raw_fd(),
                    c"self".as_ptr(),
                    target.as_mut_ptr().cast(),
                    target.len(),
                )
            };
            if length < 0 {
                return Err(io_failure(io::Error::last_os_error()));
            }
            let length = length as usize;
            self.bytes_read += length;
            if length == target.len() || self.bytes_read > MAXIMUM_TOTAL_BYTES {
                return Err(Failure::Limit);
            }
            let name = std::str::from_utf8(&target[..length]).map_err(|_| Failure::Invalid)?;
            if !number(name).is_some_and(|pid| pid > 0 && pid <= i32::MAX as u64) {
                return Err(Failure::Invalid);
            }
            self.open(proc_root, OsStr::new(name), true)
        }

        fn directory(&self, root: &File, parts: &[&str]) -> Result<File, Failure> {
            let mut current = None;
            for part in parts {
                current =
                    Some(self.open(current.as_ref().unwrap_or(root), OsStr::new(part), true)?);
            }
            current.map_or_else(|| root.try_clone().map_err(io_failure), Ok)
        }

        fn read(
            &mut self,
            directory: &File,
            name: &str,
            maximum: usize,
        ) -> Result<String, Failure> {
            let mut file = self.open(directory, OsStr::new(name), false)?;
            if !file.metadata().map_err(io_failure)?.is_file() {
                return Err(Failure::Invalid);
            }
            self.files_read += 1;
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                self.check()?;
                let allowed = maximum
                    .saturating_sub(bytes.len())
                    .min(MAXIMUM_TOTAL_BYTES.saturating_sub(self.bytes_read));
                let count = buffer.len().min(allowed + 1);
                let read = file.read(&mut buffer[..count]).map_err(io_failure)?;
                self.bytes_read += read;
                if read == 0 {
                    break;
                }
                if read > allowed {
                    return Err(Failure::Limit);
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            self.check()?;
            String::from_utf8(bytes).map_err(|_| Failure::Invalid)
        }

        fn token(&mut self, directory: &File, name: &str) -> Result<String, Failure> {
            let mut text = self.read(directory, name, MAXIMUM_TOKEN_BYTES)?;
            let token = text.trim_ascii();
            if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
                return Err(Failure::Invalid);
            }
            let start = text.len() - text.trim_ascii_start().len();
            let end = start + token.len();
            text.truncate(end);
            text.drain(..start);
            Ok(text)
        }
    }

    fn number(text: &str) -> Option<u64> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    }

    fn parts(path: &str) -> Result<Vec<&str>, Failure> {
        if !path.starts_with('/') || path.len() > MAXIMUM_PATH_BYTES || path.as_bytes().contains(&0)
        {
            return Err(Failure::Invalid);
        }
        let mut result = Vec::new();
        for component in path.split('/').filter(|part| !part.is_empty()) {
            if component == "." || component == ".." || result.len() == MAXIMUM_COMPONENTS {
                return Err(Failure::Invalid);
            }
            result.push(component);
        }
        Ok(result)
    }

    fn unescape(text: &str) -> Result<String, Failure> {
        if text.len() > MAXIMUM_PATH_BYTES {
            return Err(Failure::Limit);
        }
        let mut decoded = Vec::with_capacity(text.len());
        let mut bytes = text.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'\\' {
                let digits = [bytes.next(), bytes.next(), bytes.next()];
                let [
                    Some(a @ b'0'..=b'7'),
                    Some(b @ b'0'..=b'7'),
                    Some(c @ b'0'..=b'7'),
                ] = digits
                else {
                    return Err(Failure::Invalid);
                };
                let value = (a - b'0') as u16 * 64 + (b - b'0') as u16 * 8 + (c - b'0') as u16;
                decoded.push(u8::try_from(value).map_err(|_| Failure::Invalid)?);
            } else {
                decoded.push(byte);
            }
        }
        String::from_utf8(decoded).map_err(|_| Failure::Invalid)
    }

    fn lines<'a>(text: &'a str, reader: &Reader) -> Result<std::str::Lines<'a>, Failure> {
        reader.check()?;
        if text.lines().count() > MAXIMUM_LINES {
            return Err(Failure::Limit);
        }
        Ok(text.lines())
    }

    fn host_sample(
        reader: &mut Reader,
        proc_root: &File,
        observation: &mut Observation,
    ) -> Result<(), Failure> {
        let text = reader.read(proc_root, "meminfo", MAXIMUM_FILE_BYTES)?;
        for line in lines(&text, reader)? {
            reader.check()?;
            let mut fields = line.split_ascii_whitespace();
            let key = fields.next().unwrap_or_default();
            if key != "MemTotal:" && key != "MemAvailable:" {
                continue;
            }
            let value = fields
                .next()
                .and_then(number)
                .and_then(|value| value.checked_mul(1024));
            if fields.next() != Some("kB") || fields.next().is_some() || value.is_none() {
                return Err(Failure::Invalid);
            }
            let target = if key == "MemTotal:" {
                &mut observation.host_total_bytes
            } else {
                &mut observation.host_available_bytes
            };
            if target.replace(value.expect("checked value")).is_some() {
                return Err(Failure::Invalid);
            }
        }
        Ok(())
    }

    struct Membership {
        v1: Option<String>,
        v2: Option<String>,
        any: bool,
    }

    fn membership(reader: &mut Reader, process: &File) -> Result<Membership, Failure> {
        let text = reader.read(process, "cgroup", MAXIMUM_FILE_BYTES)?;
        let mut result = Membership {
            v1: None,
            v2: None,
            any: false,
        };
        for line in lines(&text, reader)? {
            reader.check()?;
            let Some((id, remainder)) = line.split_once(':') else {
                return Err(Failure::Invalid);
            };
            let Some((controllers, group)) = remainder.split_once(':') else {
                return Err(Failure::Invalid);
            };
            if number(id).is_none() {
                return Err(Failure::Invalid);
            }
            parts(group)?;
            result.any = true;
            let destination = if id == "0" && controllers.is_empty() {
                Some(&mut result.v2)
            } else if controllers.split(',').any(|value| value == "memory") {
                Some(&mut result.v1)
            } else {
                None
            };
            if let Some(destination) = destination
                && destination.replace(group.to_owned()).is_some()
            {
                return Err(Failure::Invalid);
            }
        }
        Ok(result)
    }

    fn cgroup(
        reader: &mut Reader,
        process: &File,
        filesystem: &File,
        observation: &mut Observation,
    ) -> Result<(), Failure> {
        let membership = membership(reader, process)?;
        observation.containment = if membership.any {
            "unresolved"
        } else {
            "unknown"
        };
        let text = reader.read(process, "mountinfo", MAXIMUM_FILE_BYTES)?;
        for line in lines(&text, reader)? {
            reader.check()?;
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let mut right = right.split_ascii_whitespace();
            let kind = right.next().unwrap_or_default();
            let source = right.next();
            let options = right.next();
            if kind != "cgroup2" && kind != "cgroup" {
                continue;
            }
            let _source = source.ok_or(Failure::Invalid)?;
            let options = options.ok_or(Failure::Invalid)?;
            let v2 = kind == "cgroup2";
            if !v2 && (kind != "cgroup" || !options.split(',').any(|value| value == "memory")) {
                continue;
            }
            let Some(group) = (if v2 {
                membership.v2.as_deref()
            } else {
                membership.v1.as_deref()
            }) else {
                continue;
            };
            let mut left = left.split_ascii_whitespace();
            if left.next().and_then(number).is_none()
                || left.next().and_then(number).is_none()
                || left.next().is_none()
            {
                return Err(Failure::Invalid);
            }
            let root = unescape(left.next().ok_or(Failure::Invalid)?)?;
            let mount = unescape(left.next().ok_or(Failure::Invalid)?)?;
            if left.next().is_none() {
                return Err(Failure::Invalid);
            }
            let root_parts = parts(&root)?;
            let group_parts = parts(group)?;
            let mount_parts = parts(&mount)?;
            let relative = if group_parts.is_empty() {
                &[][..]
            } else {
                group_parts
                    .strip_prefix(root_parts.as_slice())
                    .ok_or(Failure::Invalid)?
            };
            let mount = reader.directory(filesystem, &mount_parts)?;
            let mut directories = vec![mount];
            for component in relative {
                let next = reader.open(
                    directories.last().expect("mount directory"),
                    OsStr::new(component),
                    true,
                )?;
                directories.push(next);
            }
            observation.containment = if v2 { "cgroup-v2" } else { "cgroup-v1" };
            read_constraints(reader, &directories, v2, root_parts.is_empty(), observation)?;
            return Ok(());
        }
        Err(Failure::Invalid)
    }

    fn read_constraints(
        reader: &mut Reader,
        directories: &[File],
        v2: bool,
        hierarchy_root_visible: bool,
        observation: &mut Observation,
    ) -> Result<(), Failure> {
        observation.constraints_complete = true;
        for index in (0..directories.len()).rev() {
            reader.check()?;
            let directory = &directories[index];
            let limit = reader.token(
                directory,
                if v2 {
                    "memory.max"
                } else {
                    "memory.limit_in_bytes"
                },
            );
            match limit {
                Err(Failure::Missing) if v2 && index == 0 && hierarchy_root_visible => {
                    let usage = reader.token(directory, "memory.current");
                    let controllers =
                        reader.read(directory, "cgroup.controllers", MAXIMUM_CONTROLLERS_BYTES)?;
                    if usage != Err(Failure::Missing)
                        || !controllers
                            .split_ascii_whitespace()
                            .any(|value| value == "memory")
                    {
                        return Err(Failure::Invalid);
                    }
                }
                Err(error) => return Err(error),
                Ok(limit) => {
                    let limit = if v2 && limit == "max" {
                        None
                    } else {
                        let value = number(&limit).ok_or(Failure::Invalid)?;
                        if !v2 && value >= i64::MAX as u64 - 65535 {
                            None
                        } else {
                            Some(value)
                        }
                    };
                    let used = match reader.token(
                        directory,
                        if v2 {
                            "memory.current"
                        } else {
                            "memory.usage_in_bytes"
                        },
                    ) {
                        Ok(value) => number(&value),
                        Err(Failure::Missing | Failure::Unavailable | Failure::Invalid)
                            if limit.is_none() =>
                        {
                            None
                        }
                        Err(error) => return Err(error),
                    };
                    if limit.is_some() && used.is_none() {
                        return Err(Failure::Invalid);
                    }
                    observation.constraints.push(Constraint {
                        limit_bytes: limit,
                        used_bytes: used,
                        source: if v2 {
                            "cgroup-v2-memory"
                        } else {
                            "cgroup-v1-memory"
                        },
                    });
                }
            }
            if index == 0 {
                break;
            }
            if !v2 {
                match reader
                    .token(&directories[index - 1], "memory.use_hierarchy")?
                    .as_str()
                {
                    "0" => break,
                    "1" => {}
                    _ => return Err(Failure::Invalid),
                }
            }
        }
        Ok(())
    }

    fn process_limits(
        reader: &mut Reader,
        process: &File,
        observation: &mut Observation,
    ) -> Result<(), Failure> {
        let text = reader.read(process, "limits", MAXIMUM_FILE_BYTES)?;
        let mut address = false;
        let mut data = false;
        let mut qualified = true;
        for line in lines(&text, reader)? {
            reader.check()?;
            for (name, source, seen) in [
                ("Max address space", "linux-process-address", &mut address),
                ("Max data size", "linux-process-data", &mut data),
            ] {
                let Some(suffix) = line.strip_prefix(name) else {
                    continue;
                };
                if *seen {
                    return Err(Failure::Invalid);
                }
                *seen = true;
                let mut fields = suffix.split_ascii_whitespace();
                let soft = fields.next();
                let hard = fields.next();
                if soft != Some("unlimited")
                    || hard != Some("unlimited")
                    || fields.next() != Some("bytes")
                    || fields.next().is_some()
                {
                    qualified = false;
                    observation.constraints.push(Constraint {
                        limit_bytes: soft.and_then(number),
                        used_bytes: None,
                        source,
                    });
                }
            }
        }
        if !address || !data || !qualified {
            return Err(Failure::Invalid);
        }
        Ok(())
    }

    pub(super) fn sample_at(
        proc_path: &Path,
        filesystem_path: &Path,
        process_name: Option<&str>,
        deadline: Instant,
    ) -> Observation {
        let mut observation = Observation::unknown(Reason::HostSampleUnavailable);
        observation.source = "proc-meminfo";
        let mut reader = Reader {
            deadline,
            files_read: 0,
            bytes_read: 0,
        };
        let result = (|| {
            let proc_root = reader.root(proc_path)?;
            let filesystem = reader.root(filesystem_path)?;
            host_sample(&mut reader, &proc_root, &mut observation)?;
            let process = match process_name {
                Some(name) => reader.open(&proc_root, OsStr::new(name), true)?,
                None => reader.native_process(&proc_root)?,
            };
            if let Err(error) = cgroup(&mut reader, &process, &filesystem, &mut observation) {
                observation.reason = Some(match error {
                    Failure::Invalid => Reason::CgroupConstraintUnknown,
                    _ => error.reason(),
                });
                return Err(error);
            }
            if let Err(error) = process_limits(&mut reader, &process, &mut observation) {
                observation.containment = "linux-process-limit-unqualified";
                observation.reason = Some(match error {
                    Failure::Invalid | Failure::Missing | Failure::Unavailable => {
                        Reason::ProcessLimitsUnqualified
                    }
                    _ => error.reason(),
                });
                return Err(error);
            }
            if let (Some(total), Some(available)) = (
                observation.host_total_bytes,
                observation.host_available_bytes,
            ) && total > 0
                && available <= total
            {
                let mut effective = available;
                for constraint in &observation.constraints {
                    if let Some(limit) = constraint.limit_bytes {
                        effective =
                            effective
                                .min(limit.saturating_sub(
                                    constraint.used_bytes.ok_or(Failure::Invalid)?,
                                ));
                    }
                }
                observation.available_bytes = Some(effective);
                observation.reason = None;
            }
            Ok(())
        })();
        if let Err(error) = result {
            observation.constraints_complete = false;
            if observation.reason == Some(Reason::HostSampleUnavailable) {
                observation.reason = Some(error.reason());
            }
        }
        observation.files_read = reader.files_read;
        observation.bytes_read = reader.bytes_read as u64;
        observation
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    struct Fixture(tempfile::TempDir);

    impl Fixture {
        fn new() -> Self {
            let fixture = Self(tempfile::tempdir().unwrap());
            fixture.write("proc/meminfo", "MemTotal: 1000 kB\nMemAvailable: 400 kB\n");
            fixture.write("proc/self/limits", "Max address space unlimited unlimited bytes\nMax data size unlimited unlimited bytes\n");
            fixture
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.path().join(relative)
        }

        fn write(&self, relative: &str, value: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }

        fn v2(&self, group: &str, root: &str) {
            self.write("proc/self/cgroup", &format!("0::{group}\n"));
            self.write(
                "proc/self/mountinfo",
                &format!("31 24 0:28 {root} /sys/fs/cgroup rw - cgroup2 cgroup rw\n"),
            );
        }

        fn scope(&self, group: &str, limit: &str, used: &str) {
            let prefix = if group.is_empty() {
                "sys/fs/cgroup".to_owned()
            } else {
                format!("sys/fs/cgroup/{group}")
            };
            self.write(&format!("{prefix}/memory.max"), limit);
            self.write(&format!("{prefix}/memory.current"), used);
        }

        fn read(&self) -> Observation {
            linux::sample_at(
                &self.path("proc"),
                self.0.path(),
                Some("self"),
                Instant::now() + Duration::from_secs(2),
            )
        }

        fn tree(&self) -> Vec<(PathBuf, u32, Option<Vec<u8>>)> {
            use std::os::unix::fs::PermissionsExt;
            fn visit(
                root: &Path,
                relative: &Path,
                rows: &mut Vec<(PathBuf, u32, Option<Vec<u8>>)>,
            ) {
                let metadata = fs::symlink_metadata(root.join(relative)).unwrap();
                let contents = if metadata.is_file() {
                    Some(fs::read(root.join(relative)).unwrap())
                } else {
                    None
                };
                rows.push((relative.to_owned(), metadata.permissions().mode(), contents));
                if metadata.is_dir() {
                    for child in fs::read_dir(root.join(relative)).unwrap() {
                        visit(root, &relative.join(child.unwrap().file_name()), rows);
                    }
                }
            }
            let mut rows = Vec::new();
            visit(self.0.path(), Path::new(""), &mut rows);
            rows.sort_by(|left, right| left.0.cmp(&right.0));
            rows
        }
    }

    #[test]
    fn finite_cgroup_is_a_constraint_not_an_additional_pool_and_sampling_does_not_write() {
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        fixture.scope("", "200000", "150000");
        let before = fixture.tree();
        let sample = fixture.read();
        assert_eq!(sample.host_total_bytes, Some(1024000));
        assert_eq!(sample.host_available_bytes, Some(409600));
        assert_eq!(sample.available_bytes(), Some(50000));
        assert_eq!(sample.containment, "cgroup-v2");
        assert!(sample.constraints_complete);
        assert_eq!(sample.constraints.len(), 1);
        assert_eq!(sample.constraints[0].used_bytes, Some(150000));
        assert_eq!(fixture.tree(), before);
        assert!(sample.files_read >= 6);
        assert!(sample.bytes_read > 0);
    }

    #[test]
    fn unlimited_exhausted_and_unknown_are_distinct() {
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        for (limit, used, expected) in [
            ("max", "100", Some(409600)),
            ("100", "101", Some(0)),
            ("0", "0", Some(0)),
            ("10", "unknown", None),
        ] {
            fixture.scope("", limit, used);
            assert_eq!(
                fixture.read().available_bytes(),
                expected,
                "limit={limit}, used={used}"
            );
        }
    }

    #[test]
    fn every_v2_ancestor_has_its_own_headroom_and_unreadable_ancestor_is_unknown() {
        let fixture = Fixture::new();
        fixture.v2("/parent/child", "/");
        fixture.write("sys/fs/cgroup/cgroup.controllers", "cpu memory\n");
        fixture.scope("parent", "200000", "190000");
        fixture.scope("parent/child", "max", "100");
        assert_eq!(fixture.read().available_bytes(), Some(10000));
        fixture.scope("parent/child", "150000", "149000");
        let sample = fixture.read();
        assert_eq!(sample.available_bytes(), Some(1000));
        assert_eq!(sample.constraints.len(), 2);
        fs::remove_file(fixture.path("sys/fs/cgroup/parent/memory.current")).unwrap();
        let unknown = fixture.read();
        assert_eq!(unknown.available_bytes(), None);
        assert!(!unknown.constraints_complete);
    }

    #[test]
    fn mount_namespace_roots_are_resolved_without_escaping_the_mount() {
        let fixture = Fixture::new();
        fixture.v2("/", "/docker/container");
        fixture.scope("", "200000", "100000");
        assert_eq!(fixture.read().available_bytes(), Some(100000));
        fixture.v2("/docker/container/child", "/docker/container");
        fixture.scope("child", "80000", "79000");
        assert_eq!(fixture.read().available_bytes(), Some(1000));
        for (group, root) in [
            ("/outside", "/container"),
            ("/../escape", "/"),
            ("relative", "/"),
            ("/okay", "/../escape"),
        ] {
            fixture.v2(group, root);
            assert_eq!(
                fixture.read().available_bytes(),
                None,
                "group={group}, root={root}"
            );
        }
    }

    #[test]
    fn only_a_genuine_v2_hierarchy_root_can_lack_both_memory_interfaces() {
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        fixture.write("sys/fs/cgroup/cgroup.controllers", "cpu memory\n");
        assert_eq!(fixture.read().available_bytes(), Some(409600));
        for invalid in ["", "max trailing", "not-a-limit"] {
            fixture.write("sys/fs/cgroup/memory.max", invalid);
            assert_eq!(fixture.read().available_bytes(), None);
        }
        fs::remove_file(fixture.path("sys/fs/cgroup/memory.max")).unwrap();
        fs::create_dir(fixture.path("sys/fs/cgroup/memory.max")).unwrap();
        assert_eq!(fixture.read().available_bytes(), None);
        fs::remove_dir(fixture.path("sys/fs/cgroup/memory.max")).unwrap();
        fixture.write("sys/fs/cgroup/memory.current", "0");
        assert_eq!(fixture.read().available_bytes(), None);
        fs::remove_file(fixture.path("sys/fs/cgroup/memory.current")).unwrap();
        fixture.v2("/", "/docker/container");
        assert_eq!(fixture.read().available_bytes(), None);
    }

    #[test]
    fn numeric_overflow_malformed_usage_and_host_samples_never_create_capacity() {
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        for (limit, used) in [
            ("18446744073709551616", "1"),
            ("-1", "0"),
            ("10", "0 trailing"),
            ("10", "18446744073709551616"),
        ] {
            fixture.scope("", limit, used);
            assert_eq!(fixture.read().available_bytes(), None);
        }
        fixture.scope("", "max", "0");
        for meminfo in [
            "MemTotal: 100 kB\nMemAvailable: 200 kB\n",
            "MemTotal: 100 kB\nMemFree: 50 kB\n",
            "MemTotal: 18446744073709551615 kB\nMemAvailable: 1 kB\n",
            "MemTotal: 0 kB\nMemAvailable: 0 kB\n",
            "MemTotal: 100 kB\nMemTotal: 200 kB\nMemAvailable: 1 kB\n",
            "MemTotal: 100 kB\nMemAvailable: -1 kB\n",
        ] {
            fixture.write("proc/meminfo", meminfo);
            assert_eq!(fixture.read().available_bytes(), None);
        }
    }

    #[test]
    fn missing_ambiguous_or_malformed_membership_is_unknown() {
        let fixture = Fixture::new();
        assert_eq!(fixture.read().available_bytes(), None);
        fixture.v2("/", "/");
        fixture.scope("", "max", "0");
        for membership in [
            "",
            "0::/\n0::/different\n",
            "garbage\n",
            "0::/\nnot-valid\n",
        ] {
            fixture.write("proc/self/cgroup", membership);
            assert_eq!(fixture.read().available_bytes(), None);
        }
        fixture.v2("/", "/");
        for mountinfo in [
            "31 24 0:28 / /sys/fs/cgroup rw - cgroup2\n",
            "31 24 0:28 / /sys/fs/cgroup rw - cgroup2 cgroup\n",
            "31 24 0:28 / relative rw - cgroup2 cgroup rw\n",
        ] {
            fixture.write("proc/self/mountinfo", mountinfo);
            assert_eq!(fixture.read().available_bytes(), None);
        }
        fixture.write(
            "proc/self/mountinfo",
            "31 24 0:28 / /sys/fs/cgroup rw - ext4 disk rw\n",
        );
        assert_eq!(fixture.read().available_bytes(), None);
    }

    #[test]
    fn finite_missing_malformed_and_duplicate_process_limits_remain_unqualified() {
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        fixture.scope("", "max", "0");
        for limits in [
            "Max address space 100000 unlimited bytes\nMax data size unlimited unlimited bytes\n",
            "Max address space unlimited unlimited bytes\nMax data size 100000 unlimited bytes\n",
            "Max address space unlimited 100000 bytes\nMax data size unlimited unlimited bytes\n",
            "",
            "Max address space unlimited unlimited bytes\n",
            "Max address space invalid unlimited bytes\nMax data size unlimited unlimited bytes\n",
            "Max address space unlimited unlimited bytes\nMax data size unlimited unlimited bytes\nMax data size unlimited unlimited bytes\n",
        ] {
            fixture.write("proc/self/limits", limits);
            let sample = fixture.read();
            assert_eq!(sample.available_bytes(), None);
            assert_eq!(sample.containment, "linux-process-limit-unqualified");
            assert_eq!(sample.reason, Some(Reason::ProcessLimitsUnqualified));
            assert!(sample.constraints.len() <= 3);
        }
        fs::remove_file(fixture.path("proc/self/limits")).unwrap();
        assert_eq!(
            fixture.read().reason,
            Some(Reason::ProcessLimitsUnqualified)
        );
    }

    #[test]
    fn v1_controller_and_unlimited_sentinel_preserve_native_semantics() {
        let fixture = Fixture::new();
        fixture.write("proc/self/cgroup", "4:cpu,cpuacct:/box\n6:memory:/box\n");
        fixture.write(
            "proc/self/mountinfo",
            "31 24 0:28 /box /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n",
        );
        fixture.write("sys/fs/cgroup/memory/memory.limit_in_bytes", "200000");
        fixture.write("sys/fs/cgroup/memory/memory.usage_in_bytes", "160000");
        let sample = fixture.read();
        assert_eq!(sample.available_bytes(), Some(40000));
        assert_eq!(sample.containment, "cgroup-v1");
        fixture.write(
            "sys/fs/cgroup/memory/memory.limit_in_bytes",
            "9223372036854771712",
        );
        assert_eq!(fixture.read().available_bytes(), Some(409600));
    }

    #[test]
    fn v1_only_hierarchical_parents_constrain_the_child() {
        let fixture = Fixture::new();
        fixture.write("proc/self/cgroup", "6:memory:/parent/child\n");
        fixture.write(
            "proc/self/mountinfo",
            "31 24 0:28 / /sys/fs/cgroup rw - cgroup cgroup rw,memory\n",
        );
        for group in ["", "/parent", "/parent/child"] {
            fixture.write(
                &format!("sys/fs/cgroup{group}/memory.limit_in_bytes"),
                "9223372036854771712",
            );
            fixture.write(&format!("sys/fs/cgroup{group}/memory.usage_in_bytes"), "0");
            fixture.write(&format!("sys/fs/cgroup{group}/memory.use_hierarchy"), "1");
        }
        fixture.write("sys/fs/cgroup/parent/child/memory.limit_in_bytes", "200000");
        fixture.write("sys/fs/cgroup/parent/memory.limit_in_bytes", "100000");
        fixture.write("sys/fs/cgroup/parent/memory.usage_in_bytes", "95000");
        assert_eq!(fixture.read().available_bytes(), Some(5000));
        fixture.write("sys/fs/cgroup/parent/memory.use_hierarchy", "0");
        assert_eq!(fixture.read().available_bytes(), Some(200000));
        fixture.write("sys/fs/cgroup/parent/memory.use_hierarchy", "unknown");
        assert_eq!(fixture.read().available_bytes(), None);
    }

    #[test]
    fn escaped_mount_paths_are_decoded_but_traversal_and_symlinks_are_refused() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.write("proc/self/cgroup", "0::/\n");
        fixture.write(
            "proc/self/mountinfo",
            "31 24 0:28 / /sys/fs/cgroup\\040space rw - cgroup2 cgroup rw\n",
        );
        fixture.write("sys/fs/cgroup space/memory.max", "100");
        fixture.write("sys/fs/cgroup space/memory.current", "20");
        assert_eq!(fixture.read().available_bytes(), Some(80));
        fixture.write(
            "proc/self/mountinfo",
            "31 24 0:28 / /sys/fs/\\056\\056/escape rw - cgroup2 cgroup rw\n",
        );
        assert_eq!(fixture.read().available_bytes(), None);
        fixture.v2("/", "/");
        symlink("cgroup space", fixture.path("sys/fs/cgroup")).unwrap();
        let before = fixture.tree();
        assert_eq!(fixture.read().available_bytes(), None);
        assert_eq!(fixture.tree(), before);
        fs::remove_file(fixture.path("sys/fs/cgroup")).unwrap();
        fixture.scope("", "100", "20");
        fs::remove_file(fixture.path("sys/fs/cgroup/memory.max")).unwrap();
        symlink(
            "../cgroup space/memory.max",
            fixture.path("sys/fs/cgroup/memory.max"),
        )
        .unwrap();
        assert_eq!(fixture.read().available_bytes(), None);
    }

    #[test]
    fn special_files_deadlines_and_bounded_metadata_fail_closed_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        fixture.scope("", "100", "20");
        let expired = linux::sample_at(
            &fixture.path("proc"),
            fixture.0.path(),
            Some("self"),
            Instant::now(),
        );
        assert_eq!(expired.reason, Some(Reason::Deadline));
        assert_eq!(expired.files_read, 0);
        fs::remove_file(fixture.path("sys/fs/cgroup/memory.max")).unwrap();
        let fifo = CString::new(
            fixture
                .path("sys/fs/cgroup/memory.max")
                .as_os_str()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(fixture.read().available_bytes(), None);
        fs::remove_file(fixture.path("sys/fs/cgroup/memory.max")).unwrap();
        fixture.write("sys/fs/cgroup/memory.max", &"1".repeat(257));
        assert_eq!(fixture.read().reason, Some(Reason::ReadLimit));
        fixture.write("proc/meminfo", &" ".repeat(1024 * 1024 + 1));
        let too_large = fixture.read();
        assert_eq!(too_large.reason, Some(Reason::ReadLimit));
        assert!(too_large.bytes_read <= 1024 * 1024 + 1);
        fixture.write("proc/meminfo", "MemTotal: 1000 kB\nMemAvailable: 400 kB\n");
        fixture.write("proc/self/cgroup", &format!("0::/{}\n", "a".repeat(4097)));
        assert_eq!(fixture.read().available_bytes(), None);
        fixture.write(
            "proc/self/cgroup",
            &format!("0::/{}\n", vec!["a"; 129].join("/")),
        );
        assert_eq!(fixture.read().available_bytes(), None);
    }

    #[test]
    fn proc_self_selects_the_procfs_namespace_pid_and_cannot_redirect_to_another_path() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        fixture.scope("", "100", "20");
        fs::rename(fixture.path("proc/self"), fixture.path("proc/424242")).unwrap();
        symlink("424242", fixture.path("proc/self")).unwrap();
        let sample = || {
            linux::sample_at(
                &fixture.path("proc"),
                fixture.0.path(),
                None,
                Instant::now() + Duration::from_secs(1),
            )
        };
        let before = fixture.tree();
        assert_eq!(sample().available_bytes(), Some(80));
        assert_eq!(fixture.tree(), before);
        for invalid in [
            "../424242",
            "/424242",
            "not-a-pid",
            "0",
            "2147483648",
            "1234567890123456789012345678901234567890",
        ] {
            fs::remove_file(fixture.path("proc/self")).unwrap();
            symlink(invalid, fixture.path("proc/self")).unwrap();
            assert_eq!(sample().available_bytes(), None);
        }
        fs::remove_file(fixture.path("proc/self")).unwrap();
        symlink("424243", fixture.path("proc/self")).unwrap();
        symlink("424242", fixture.path("proc/424243")).unwrap();
        assert_eq!(sample().available_bytes(), None);
    }

    #[test]
    fn aggregate_reads_and_line_counts_are_bounded_across_individually_valid_files() {
        let fixture = Fixture::new();
        fixture.v2("/", "/");
        fixture.scope("", "100", "20");
        let maximum = 1024 * 1024;
        let padded = |text: &str| format!("{text}{}\n", "x".repeat(maximum - text.len() - 1));
        fixture.write(
            "proc/meminfo",
            &padded("MemTotal: 1000 kB\nMemAvailable: 400 kB\nIgnored: "),
        );
        let mut membership = "0::/\n".to_owned();
        let row = format!("1:cpu:/{}\n", "a".repeat(4000));
        while membership.len() + row.len() + 16 < maximum {
            membership.push_str(&row);
        }
        let remaining = maximum - membership.len();
        membership.push_str(&format!("1:cpu:/{}\n", "b".repeat(remaining - 8)));
        assert_eq!(membership.len(), maximum);
        fixture.write("proc/self/cgroup", &membership);
        fixture.write("proc/self/mountinfo", &padded("31 24 0:28 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n31 24 0:28 / /other rw - ext4 disk "));
        fixture.write("proc/self/limits", &padded("Max address space unlimited unlimited bytes\nMax data size unlimited unlimited bytes\nIgnored: "));
        let before = fixture.tree();
        let sample = fixture.read();
        assert_eq!(sample.available_bytes(), None);
        assert_eq!(sample.reason, Some(Reason::ReadLimit));
        assert_eq!(sample.bytes_read, 4 * maximum as u64 + 1);
        assert_eq!(fixture.tree(), before);
        fixture.write("proc/meminfo", &"ignored\n".repeat(16385));
        assert_eq!(fixture.read().reason, Some(Reason::ReadLimit));
    }

    #[test]
    #[ignore = "opt-in actual host observation; does not qualify hardware or protected execution"]
    fn actual_host_observation() {
        let sample = super::sample(Instant::now() + Duration::from_secs(2));
        println!("{}", serde_json::to_string(&sample).unwrap());
        assert_eq!(sample.source, "proc-meminfo");
        assert_eq!(sample.available_bytes().is_some(), sample.reason.is_none());
        if let Some(available) = sample.available_bytes() {
            assert!(sample.constraints_complete);
            assert!(available <= sample.host_available_bytes.unwrap());
            assert!(sample.host_available_bytes.unwrap() <= sample.host_total_bytes.unwrap());
        }
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod platform_tests {
    #[test]
    fn unsupported_platform_does_not_infer_capacity() {
        let sample = super::sample(std::time::Instant::now());
        assert_eq!(sample.available_bytes(), None);
        assert_eq!(sample.reason, Some(super::Reason::UnsupportedPlatform));
        assert_eq!(sample.files_read, 0);
    }
}
