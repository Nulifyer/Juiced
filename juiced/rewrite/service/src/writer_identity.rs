use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, time::Instant};

const METHOD: &str = "linux-pidfs-64-v1";
const MAXIMUM_IDENTITY_BYTES: usize = 1024;

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct WriterIdentity {
    version: u32,
    method: String,
    pid: u32,
    domain: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityWire {
    version: u32,
    #[serde(deserialize_with = "bounded_text")]
    method: String,
    pid: u32,
    #[serde(deserialize_with = "bounded_text")]
    domain: String,
    #[serde(deserialize_with = "bounded_text")]
    token: String,
}

fn bounded_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    struct Text;
    impl serde::de::Visitor<'_> for Text {
        type Value = String;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded writer identity text")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            if value.len() > 64 {
                Err(E::custom("Invalid snapshot writer identity"))
            } else {
                Ok(value.to_owned())
            }
        }
    }
    deserializer.deserialize_str(Text)
}

impl<'de> Deserialize<'de> for WriterIdentity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = IdentityWire::deserialize(deserializer)
            .map_err(|_| serde::de::Error::custom("Invalid snapshot writer identity"))?;
        if wire.version != 1
            || wire.pid == 0
            || wire.pid > i32::MAX as u32
            || wire.method.is_empty()
            || wire.method.len() > 64
            || !wire.method.bytes().all(|byte| byte.is_ascii_graphic())
            || !digest(&wire.domain)
            || !digest(&wire.token)
        {
            return Err(serde::de::Error::custom("Invalid snapshot writer identity"));
        }
        Ok(Self {
            version: wire.version,
            method: wire.method,
            pid: wire.pid,
            domain: wire.domain,
            token: wire.token,
        })
    }
}

impl WriterIdentity {
    pub fn decode(bytes: &[u8]) -> Result<Self, Reason> {
        if bytes.len() > MAXIMUM_IDENTITY_BYTES {
            return Err(Reason::InvalidIdentity);
        }
        serde_json::from_slice(bytes).map_err(|_| Reason::InvalidIdentity)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl fmt::Debug for WriterIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WriterIdentity { private process facts }")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Exited,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Captured,
    Running,
    ProcessExited,
    ProcessAbsent,
    ProcessReplaced,
    InvalidIdentity,
    UnsupportedPlatform,
    UnsupportedMethod,
    UnsupportedPidfs,
    DomainUnavailable,
    DomainChanged,
    ProcessUnavailable,
    ProbeUnavailable,
    OwnershipChanged,
    Cancelled,
    Deadline,
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Captured => "Writer identity captured",
            Self::Running => "Original direct process remains live",
            Self::ProcessExited => "Original direct process exited",
            Self::ProcessAbsent => "Original direct process is absent",
            Self::ProcessReplaced => "PID identifies a different process object",
            Self::InvalidIdentity => "Invalid snapshot writer identity",
            Self::UnsupportedPlatform => "Writer identity is unsupported on this platform",
            Self::UnsupportedMethod => "Writer identity method is unsupported",
            Self::UnsupportedPidfs => "Kernel process-object identity is unavailable",
            Self::DomainUnavailable => "Process observation domain is unavailable",
            Self::DomainChanged => "Process observation domain changed",
            Self::ProcessUnavailable => "Direct process identity is unavailable",
            Self::ProbeUnavailable => "Direct process observation is inconclusive",
            Self::OwnershipChanged => "Owned process generation changed",
            Self::Cancelled => "Writer identity observation was cancelled",
            Self::Deadline => "Writer identity observation deadline elapsed",
        })
    }
}
impl std::error::Error for Reason {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub state: State,
    pub reason: Reason,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct Control<'a, F> {
    deadline: Instant,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Control<'_, F> {
    fn check(&self) -> Result<(), Reason> {
        if (self.cancelled)() {
            Err(Reason::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Reason::Deadline)
        } else {
            Ok(())
        }
    }
}

// The caller must retain its exact child owner throughout both callback checks.
// A numeric PID or an executable observation alone cannot establish ownership.
pub fn capture_owned(
    pid: u32,
    generation: u64,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
    mut still_owned: impl FnMut(u32, u64) -> bool,
) -> Result<WriterIdentity, Reason> {
    let control = Control {
        deadline,
        cancelled: &cancelled,
    };
    control.check()?;
    if pid == 0 || pid > i32::MAX as u32 || generation == 0 {
        return Err(Reason::InvalidIdentity);
    }
    if !still_owned(pid, generation) {
        return Err(Reason::OwnershipChanged);
    }
    control.check()?;
    let identity = platform::capture(pid, &control)?;
    if !still_owned(pid, generation) {
        return Err(Reason::OwnershipChanged);
    }
    control.check()?;
    if platform::domain(&control)? != identity.domain {
        return Err(Reason::DomainChanged);
    }
    control.check()?;
    Ok(identity)
}

pub fn observe(
    identity: &WriterIdentity,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Observation {
    let control = Control {
        deadline,
        cancelled: &cancelled,
    };
    let result = control.check().and_then(|()| {
        if identity.method != METHOD {
            return Err(Reason::UnsupportedMethod);
        }
        let domain = platform::domain(&control)?;
        if domain != identity.domain {
            return Err(Reason::DomainChanged);
        }
        let observed = platform::observe(identity, &control)?;
        if platform::domain(&control)? != domain {
            return Err(Reason::DomainChanged);
        }
        control.check()?;
        Ok(observed)
    });
    result.unwrap_or_else(|reason| Observation {
        state: State::Unknown,
        reason,
    })
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        fs::File,
        io::{self, Read},
        os::fd::{AsRawFd, FromRawFd, RawFd},
    };

    fn open(path: &std::ffi::CStr, flags: i32) -> Result<File, Reason> {
        let descriptor = unsafe { libc::open(path.as_ptr(), flags) };
        if descriptor < 0 {
            Err(Reason::DomainUnavailable)
        } else {
            Ok(unsafe { File::from_raw_fd(descriptor) })
        }
    }

    fn filesystem(descriptor: RawFd) -> Result<libc::c_long, Reason> {
        let mut value = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(descriptor, value.as_mut_ptr()) } != 0 {
            return Err(Reason::ProbeUnavailable);
        }
        Ok(unsafe { value.assume_init() }.f_type)
    }

    fn inode(descriptor: RawFd) -> Result<String, Reason> {
        let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(descriptor, value.as_mut_ptr()) } != 0 {
            return Err(Reason::ProbeUnavailable);
        }
        let value = unsafe { value.assume_init() };
        if value.st_ino == 0 {
            return Err(Reason::ProbeUnavailable);
        }
        Ok(format!("{}:{}", value.st_dev, value.st_ino))
    }

    pub(super) fn domain<F: Fn() -> bool>(control: &Control<'_, F>) -> Result<String, Reason> {
        identity_width(
            std::mem::size_of::<usize>(),
            std::mem::size_of::<libc::ino_t>(),
        )?;
        control.check()?;
        let mut filesystem_type = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(c"/proc".as_ptr(), filesystem_type.as_mut_ptr()) } != 0
            || unsafe { filesystem_type.assume_init() }.f_type != libc::PROC_SUPER_MAGIC
        {
            return Err(Reason::DomainUnavailable);
        }
        let mut self_pid = [0u8; 64];
        let count = unsafe {
            libc::readlink(
                c"/proc/self".as_ptr(),
                self_pid.as_mut_ptr().cast(),
                self_pid.len(),
            )
        };
        if count <= 0
            || self_pid.get(..count as usize) != Some(std::process::id().to_string().as_bytes())
        {
            return Err(Reason::DomainUnavailable);
        }
        control.check()?;
        let mut boot = open(
            c"/proc/sys/kernel/random/boot_id",
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW,
        )?;
        if filesystem(boot.as_raw_fd())? != libc::PROC_SUPER_MAGIC {
            return Err(Reason::DomainUnavailable);
        }
        let mut bytes = [0u8; 128];
        let count = boot
            .read(&mut bytes)
            .map_err(|_| Reason::DomainUnavailable)?;
        if count != 37 || bytes[36] != b'\n' || !uuid(&bytes[..36]) {
            return Err(Reason::DomainUnavailable);
        }
        control.check()?;
        // Namespace entries are kernel links; O_NOFOLLOW would reject their handles.
        let namespace = open(c"/proc/self/ns/pid", libc::O_RDONLY | libc::O_CLOEXEC)?;
        if filesystem(namespace.as_raw_fd())? != libc::NSFS_MAGIC {
            return Err(Reason::DomainUnavailable);
        }
        let namespace = inode(namespace.as_raw_fd())?;
        control.check()?;
        let mut hash = Sha256::new();
        hash.update(b"linux-pidfs-64-v1\n");
        hash.update(&bytes[..36]);
        hash.update(b"\n");
        hash.update(namespace.as_bytes());
        Ok(format!("{:x}", hash.finalize()))
    }

    fn uuid(bytes: &[u8]) -> bool {
        bytes.len() == 36
            && bytes.iter().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }

    fn pidfd(pid: u32) -> io::Result<File> {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if descriptor < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(descriptor as RawFd) })
        }
    }

    fn pidfd_error(error: &io::Error) -> Reason {
        if error.raw_os_error() == Some(libc::ENOSYS) {
            Reason::UnsupportedPidfs
        } else {
            Reason::ProcessUnavailable
        }
    }

    fn token(file: &File) -> Result<String, Reason> {
        const PIDFS_MAGIC: libc::c_long = 0x50494446;
        // Anonymous-inode pidfds and truncated inode values cannot prove process identity.
        identity_width(
            std::mem::size_of::<usize>(),
            std::mem::size_of::<libc::ino_t>(),
        )?;
        if filesystem(file.as_raw_fd())? != PIDFS_MAGIC {
            return Err(Reason::UnsupportedPidfs);
        }
        Ok(format!("{:x}", Sha256::digest(inode(file.as_raw_fd())?)))
    }

    fn identity_width(pointer_bytes: usize, inode_bytes: usize) -> Result<(), Reason> {
        // A 32-bit kernel can recycle pidfs inode numbers even with wide libc inode fields.
        if pointer_bytes < 8 || inode_bytes < 8 {
            Err(Reason::UnsupportedPidfs)
        } else {
            Ok(())
        }
    }

    pub(super) fn capture<F: Fn() -> bool>(
        pid: u32,
        control: &Control<'_, F>,
    ) -> Result<WriterIdentity, Reason> {
        let domain = domain(control)?;
        control.check()?;
        let file = pidfd(pid).map_err(|error| pidfd_error(&error))?;
        control.check()?;
        let token = token(&file)?;
        control.check()?;
        Ok(WriterIdentity {
            version: 1,
            method: METHOD.to_owned(),
            pid,
            domain,
            token,
        })
    }

    pub(super) fn observe<F: Fn() -> bool>(
        identity: &WriterIdentity,
        control: &Control<'_, F>,
    ) -> Result<Observation, Reason> {
        control.check()?;
        let file = match pidfd(identity.pid) {
            Ok(file) => file,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                return Ok(Observation {
                    state: State::Exited,
                    reason: Reason::ProcessAbsent,
                });
            }
            Err(error) => return Err(pidfd_error(&error)),
        };
        control.check()?;
        let token = token(&file)?;
        control.check()?;
        let mut event = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut event, 1, 0) } < 0 {
            return Err(Reason::ProbeUnavailable);
        }
        classify(&identity.token, &token, event.revents)
    }

    fn classify(expected: &str, actual: &str, events: i16) -> Result<Observation, Reason> {
        if events & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(Reason::ProbeUnavailable);
        }
        let reason = if actual != expected {
            Reason::ProcessReplaced
        } else if events & (libc::POLLIN | libc::POLLHUP) != 0 {
            Reason::ProcessExited
        } else {
            Reason::Running
        };
        Ok(Observation {
            state: if reason == Reason::Running {
                State::Running
            } else {
                State::Exited
            },
            reason,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn token_reuse_and_poll_errors_are_distinct() {
            assert_eq!(
                classify("a", "b", 0).unwrap().reason,
                Reason::ProcessReplaced
            );
            for event in [libc::POLLERR, libc::POLLNVAL, libc::POLLERR | libc::POLLIN] {
                assert_eq!(classify("a", "b", event), Err(Reason::ProbeUnavailable));
            }
            for event in [libc::POLLIN, libc::POLLHUP] {
                assert_eq!(classify("a", "a", event).unwrap().state, State::Exited);
            }
            assert_eq!(classify("a", "a", 0).unwrap().state, State::Running);
        }

        #[test]
        fn boot_uuid_shape_is_exact() {
            assert!(uuid(b"01234567-89ab-cdef-0123-456789abcdef"));
            assert!(!uuid(b"01234567/89ab-cdef-0123-456789abcdef"));
            assert!(!uuid(b"01234567-89ab-cdef-0123-456789abcdeg"));
        }

        #[test]
        fn regular_file_inode_is_not_process_identity() {
            let file = tempfile::tempfile().unwrap();
            assert_eq!(token(&file), Err(Reason::UnsupportedPidfs));
        }

        #[test]
        fn absent_syscall_is_unsupported_but_other_errors_are_inconclusive() {
            assert_eq!(
                pidfd_error(&io::Error::from_raw_os_error(libc::ENOSYS)),
                Reason::UnsupportedPidfs
            );
            for error in [libc::EACCES, libc::EPERM, libc::EINVAL, libc::ESRCH] {
                assert_eq!(
                    pidfd_error(&io::Error::from_raw_os_error(error)),
                    Reason::ProcessUnavailable
                );
            }
        }

        #[test]
        fn wide_inode_type_does_not_qualify_32_bit_process_identity() {
            for (pointer_bytes, inode_bytes) in [(4, 8), (8, 4), (4, 4)] {
                assert_eq!(
                    identity_width(pointer_bytes, inode_bytes),
                    Err(Reason::UnsupportedPidfs)
                );
            }
            assert_eq!(identity_width(8, 8), Ok(()));
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub(super) fn domain<F: Fn() -> bool>(_: &Control<'_, F>) -> Result<String, Reason> {
        Err(Reason::UnsupportedPlatform)
    }
    pub(super) fn capture<F: Fn() -> bool>(
        _: u32,
        _: &Control<'_, F>,
    ) -> Result<WriterIdentity, Reason> {
        Err(Reason::UnsupportedPlatform)
    }
    pub(super) fn observe<F: Fn() -> bool>(
        _: &WriterIdentity,
        _: &Control<'_, F>,
    ) -> Result<Observation, Reason> {
        Err(Reason::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn wire() -> serde_json::Value {
        json!({"version":1,"method":METHOD,"pid":123,"domain":"a".repeat(64),"token":"b".repeat(64)})
    }

    #[test]
    fn strict_persisted_identity_roundtrip_and_redaction() {
        let identity: WriterIdentity = serde_json::from_value(wire()).unwrap();
        assert_eq!(serde_json::to_value(&identity).unwrap(), wire());
        assert_eq!(identity.pid(), 123);
        assert!(!format!("{identity:?}").contains(&"a".repeat(64)));
        for (key, value) in [
            ("version", json!(2)),
            ("pid", json!(0)),
            ("pid", json!(2147483648u64)),
            ("method", json!("")),
            ("method", json!("a".repeat(65))),
            ("method", json!("private\ntext")),
            ("domain", json!("A".repeat(64))),
            ("token", json!("../private")),
            ("extra", json!(true)),
        ] {
            let mut invalid = wire();
            invalid[key] = value;
            assert!(serde_json::from_value::<WriterIdentity>(invalid).is_err());
        }
        let encoded = serde_json::to_string(&wire()).unwrap();
        assert!(
            WriterIdentity::decode(format!("{{\"version\":1,{}", &encoded[1..]).as_bytes())
                .is_err()
        );
        assert!(WriterIdentity::decode(&[b' '; MAXIMUM_IDENTITY_BYTES + 1]).is_err());
        let error =
            serde_json::from_str::<WriterIdentity>("{\"private-source-path\":true}").unwrap_err();
        assert!(!error.to_string().contains("private-source-path"));
    }

    #[test]
    fn unsupported_method_cancellation_and_deadline_never_prove_exit() {
        let mut value = wire();
        value["method"] = json!("future-private-method");
        let identity: WriterIdentity = serde_json::from_value(value).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            observe(&identity, deadline, || false).reason,
            Reason::UnsupportedMethod
        );
        assert_eq!(observe(&identity, deadline, || true).state, State::Unknown);
        assert_eq!(
            observe(&identity, Instant::now(), || false).reason,
            Reason::Deadline
        );
    }

    #[test]
    fn capture_requires_nonzero_exact_owned_generation_before_io() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            capture_owned(123, 0, deadline, || false, |_, _| panic!()),
            Err(Reason::InvalidIdentity)
        );
        let mut calls = Vec::new();
        assert_eq!(
            capture_owned(
                123,
                7,
                deadline,
                || false,
                |pid, generation| {
                    calls.push((pid, generation));
                    false
                }
            ),
            Err(Reason::OwnershipChanged)
        );
        assert_eq!(calls, [(123, 7)]);
    }

    #[test]
    fn cancelled_capture_does_not_invoke_ownership_or_process_probe() {
        assert_eq!(
            capture_owned(
                123,
                1,
                Instant::now() + Duration::from_secs(1),
                || true,
                |_, _| panic!("cancelled capture must not inspect the child")
            ),
            Err(Reason::Cancelled)
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_platform_is_unknown() {
        let identity: WriterIdentity = serde_json::from_value(wire()).unwrap();
        assert_eq!(
            observe(&identity, Instant::now() + Duration::from_secs(1), || false),
            Observation {
                state: State::Unknown,
                reason: Reason::UnsupportedPlatform
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn changed_domain_is_unknown_before_pid_lookup() {
        let mut identity: WriterIdentity = serde_json::from_value(wire()).unwrap();
        identity.domain = "0".repeat(64);
        let observed = observe(&identity, Instant::now() + Duration::from_secs(1), || false);
        assert_eq!(observed.state, State::Unknown);
        assert!(matches!(
            observed.reason,
            Reason::DomainChanged | Reason::DomainUnavailable | Reason::ProbeUnavailable
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires positive Linux pidfs capability and an owned child process"]
    fn actual_owned_identity_live_exit_and_no_reap() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read line; exit 0"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        let captured = capture_owned(
            pid,
            17,
            deadline,
            || false,
            |observed_pid, generation| {
                observed_pid == child.id()
                    && generation == 17
                    && child.try_wait().unwrap().is_none()
            },
        );
        if captured.is_err() {
            child.stdin.take();
            child.wait().unwrap();
        }
        let identity =
            captured.expect("positive Linux pidfs capability is required for this opt-in test");
        assert_eq!(observe(&identity, deadline, || false).state, State::Running);
        let repeated = capture_owned(
            pid,
            17,
            deadline,
            || false,
            |observed_pid, generation| {
                observed_pid == child.id()
                    && generation == 17
                    && child.try_wait().unwrap().is_none()
            },
        )
        .unwrap();
        assert_eq!(identity, repeated);
        let mut callbacks = 0;
        assert_eq!(
            capture_owned(
                pid,
                17,
                deadline,
                || false,
                |observed_pid, generation| {
                    callbacks += 1;
                    observed_pid == child.id() && generation == 17 && callbacks == 1
                }
            ),
            Err(Reason::OwnershipChanged)
        );
        assert_eq!(callbacks, 2);
        child.stdin.take();
        loop {
            if observe(&identity, deadline, || false).state == State::Exited {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(
            child.wait().unwrap().success(),
            "observation must not reap the owned child"
        );
        assert_eq!(observe(&identity, deadline, || false).state, State::Exited);
    }
}
