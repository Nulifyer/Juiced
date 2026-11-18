mod checksums;

pub use checksums::{FILENAME as CHECKSUMS_FILENAME, FileChecksums};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

pub struct VerifiedFile {
    path: PathBuf,
    file: File,
    digest: String,
    bytes: u64,
    modified: SystemTime,
}

impl VerifiedFile {
    pub fn open(path: &Path, deadline: Instant) -> io::Result<Self> {
        Self::open_remembered(path, deadline, None)
    }

    /// Like `open`, and also reuses and records the digest in `checksums`, which outlives
    /// this process.
    pub fn open_remembered(
        path: &Path,
        deadline: Instant,
        checksums: Option<&FileChecksums>,
    ) -> io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000);
        }
        let file = options.open(&path)?;
        Self::from_file_checked(&path, file, Some(deadline), checksums, || Ok(()))
    }

    /// Reads the whole file unless its digest is remembered for its current stamp. A reused
    /// digest still comes with the open handle and the same `verify_current` checks.
    pub(crate) fn from_file_checked(
        path: &Path,
        mut file: File,
        deadline: Option<Instant>,
        checksums: Option<&FileChecksums>,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Self> {
        check()?;
        let before = file.metadata()?;
        if !before.is_file() || before.file_type().is_symlink() {
            return Err(io::Error::other("Identity requires a regular file"));
        }
        if let Some(digest) = remembered_digest(path, &before, checksums) {
            let verified = Self {
                path: path.to_owned(),
                file,
                digest,
                bytes: before.len(),
                modified: before.modified()?,
            };
            verified.verify_current()?;
            return Ok(verified);
        }
        let began = begin_read(&before);
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() > deadline) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Identity hashing exceeded its deadline",
                ));
            }
            check()?;
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes = bytes
                .checked_add(read as u64)
                .ok_or_else(|| io::Error::other("Identity size overflow"))?;
            hash.update(&buffer[..read]);
        }
        let verified = Self {
            path: path.to_owned(),
            file,
            digest: format!("{:x}", hash.finalize()),
            bytes,
            modified: before.modified()?,
        };
        if bytes != before.len() {
            return Err(io::Error::other(
                "The identity source changed during hashing",
            ));
        }
        check()?;
        verified.verify_current()?;
        remember_read(
            path,
            &before,
            &verified.file.metadata()?,
            began,
            &verified.digest,
            checksums,
        );
        Ok(verified)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub(crate) fn metadata_reader(&self) -> io::Result<File> {
        self.verify_current()?;
        self.file.try_clone()
    }

    pub fn verify_current(&self) -> io::Result<()> {
        let after = self.file.metadata()?;
        if after.len() != self.bytes
            || after.modified()? != self.modified
            || !crate::storage::same_file(&self.file, &self.path)?
        {
            return Err(io::Error::other("The retained identity source changed"));
        }
        Ok(())
    }
}

pub fn file_digest(path: &Path, deadline: Instant) -> io::Result<String> {
    #[cfg(unix)]
    if let Some(digest) = stamp::cached(&stamp::Stamp::of(&std::fs::metadata(
        std::fs::canonicalize(path)?,
    )?)) {
        return Ok(digest);
    }
    Ok(VerifiedFile::open(path, deadline)?.digest)
}

pub(crate) fn cached_digest(metadata: &Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        stamp::cached(&stamp::Stamp::of(metadata))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

pub(crate) fn remember_digest(metadata: &Metadata, digest: &str) {
    #[cfg(unix)]
    stamp::remember(stamp::Stamp::of(metadata), digest);
    #[cfg(not(unix))]
    let _ = (metadata, digest);
}

/// The digest an earlier full read found for the file at `path` with exactly this stamp.
pub(crate) fn remembered_digest(
    path: &Path,
    metadata: &Metadata,
    checksums: Option<&FileChecksums>,
) -> Option<String> {
    #[cfg(unix)]
    {
        let stamp = stamp::Stamp::of(metadata);
        if let Some(digest) = stamp::cached(&stamp) {
            if let Some(checksums) = checksums {
                checksums.record(path, &stamp, &digest);
            }
            return Some(digest);
        }
        let digest = checksums?.lookup(path, &stamp)?;
        stamp::remember(stamp, &digest);
        Some(digest)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata, checksums);
        None
    }
}

/// Call before reading a whole file for its digest. A file changed within the last timestamp
/// tick could change again without a new stamp, so this waits the tick out first.
pub(crate) fn begin_read(metadata: &Metadata) -> SystemTime {
    #[cfg(test)]
    FULL_READS.with(|reads| reads.set(reads.get() + 1));
    #[cfg(unix)]
    if let Some(wait) = stamp::Stamp::of(metadata).unsettled(SystemTime::now())
        && wait <= stamp::TICK
    {
        std::thread::sleep(wait);
    }
    #[cfg(not(unix))]
    let _ = metadata;
    SystemTime::now()
}

/// Remembers the digest of a full read that began at `began`, when the file kept one settled
/// stamp throughout.
pub(crate) fn remember_read(
    path: &Path,
    before: &Metadata,
    after: &Metadata,
    began: SystemTime,
    digest: &str,
    checksums: Option<&FileChecksums>,
) {
    #[cfg(unix)]
    {
        let stamp = stamp::Stamp::of(before);
        if stamp != stamp::Stamp::of(after) || stamp.unsettled(began).is_some() {
            return;
        }
        if let Some(checksums) = checksums {
            checksums.record(path, &stamp, digest);
        }
        stamp::remember(stamp, digest);
    }
    #[cfg(not(unix))]
    let _ = (path, before, after, began, digest, checksums);
}

#[cfg(test)]
thread_local! {
    static FULL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whole-file reads this thread has started.
#[cfg(test)]
pub(crate) fn full_reads() -> usize {
    FULL_READS.with(std::cell::Cell::get)
}

/// Drops what this process remembers about one file, as a restart would.
#[cfg(test)]
pub(crate) fn forget_in_process(path: &Path) {
    #[cfg(unix)]
    stamp::forget(&stamp::Stamp::of(&std::fs::metadata(path).unwrap()));
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(unix)]
mod stamp {
    use std::{
        collections::HashMap,
        os::unix::fs::MetadataExt,
        sync::{Mutex, OnceLock},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    /// Large enough for the TheNoise bundle's ~30,000 files, so a launch does not rehash them.
    const ENTRIES: usize = 65536;

    /// Longer than the kernel tick that file timestamps advance by.
    pub(super) const TICK: Duration = Duration::from_millis(20);
    /// For filesystems that keep whole seconds, or two as FAT does.
    const COARSE_TICK: Duration = Duration::from_secs(2);

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    pub(super) struct Stamp {
        pub(super) device: u64,
        pub(super) inode: u64,
        pub(super) bytes: u64,
        pub(super) modified: (i64, i64),
        pub(super) changed: (i64, i64),
    }

    impl Stamp {
        pub(super) fn of(metadata: &std::fs::Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            }
        }

        /// How long after `now` a change could still leave these timestamps as they are.
        /// `None` once every later change must produce a different stamp.
        pub(super) fn unsettled(&self, now: SystemTime) -> Option<Duration> {
            let tick = if self.modified.1 == 0 && self.changed.1 == 0 {
                COARSE_TICK
            } else {
                TICK
            };
            let latest = self.modified.max(self.changed);
            let seconds = u64::try_from(latest.0).ok()?;
            let settled =
                UNIX_EPOCH + Duration::new(seconds, u32::try_from(latest.1).unwrap_or(0)) + tick;
            match settled.duration_since(now) {
                Ok(wait) if !wait.is_zero() => Some(wait),
                Ok(_) => Some(Duration::from_nanos(1)),
                Err(_) => None,
            }
        }
    }

    fn digests() -> &'static Mutex<HashMap<Stamp, String>> {
        static DIGESTS: OnceLock<Mutex<HashMap<Stamp, String>>> = OnceLock::new();
        DIGESTS.get_or_init(Mutex::default)
    }

    pub(super) fn cached(stamp: &Stamp) -> Option<String> {
        digests().lock().ok()?.get(stamp).cloned()
    }

    pub(super) fn remember(stamp: Stamp, digest: &str) {
        if let Ok(mut digests) = digests().lock() {
            if digests.len() >= ENTRIES {
                digests.clear();
            }
            digests.insert(stamp, digest.to_owned());
        }
    }

    #[cfg(test)]
    pub(super) fn forget(stamp: &Stamp) {
        digests().lock().unwrap().remove(stamp);
    }
}

pub fn executable_path(command: &str) -> io::Result<PathBuf> {
    let path = Path::new(command);
    if path.is_absolute() || path.components().count() > 1 {
        return std::fs::canonicalize(path);
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return std::fs::canonicalize(candidate);
        }
        #[cfg(windows)]
        if candidate.with_extension("exe").is_file() {
            return std::fs::canonicalize(candidate.with_extension("exe"));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Engine executable was not found",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn repeated_digests_reuse_unchanged_stamps_and_rehash_rewritten_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights.bin");
        std::fs::write(&path, b"first content").unwrap();
        let first = file_digest(&path, deadline()).unwrap();
        assert_eq!(first, sha(b"first content"));
        assert_eq!(file_digest(&path, deadline()).unwrap(), first);
        std::fs::write(&path, b"other content").unwrap();
        assert_eq!(
            file_digest(&path, deadline()).unwrap(),
            sha(b"other content")
        );
        let verified = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(file_digest(&path, deadline()).unwrap(), verified.digest());
    }

    #[cfg(unix)]
    #[test]
    fn an_unchanged_file_is_read_once_and_a_new_time_size_inode_or_content_is_read_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights.bin");
        std::fs::write(&path, b"first content").unwrap();
        let reads = full_reads();
        let first = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 1);
        let reused = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 1);
        assert_eq!(reused.digest(), sha(b"first content"));
        assert_eq!(reused.bytes(), 13);
        reused.verify_current().unwrap();
        assert_eq!(file_digest(&path, deadline()).unwrap(), first.digest());
        assert_eq!(full_reads(), reads + 1);

        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        assert!(reused.verify_current().is_err());
        let retimed = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 2);
        assert_eq!(retimed.digest(), first.digest());

        std::fs::write(&path, b"first content, longer").unwrap();
        let resized = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 3);
        assert_eq!(resized.digest(), sha(b"first content, longer"));

        let copy = directory.path().join("copy.bin");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::rename(&copy, &path).unwrap();
        let replaced = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 4);
        assert_eq!(replaced.digest(), resized.digest());
        assert!(resized.verify_current().is_err());

        std::fs::write(&path, b"other content, longer").unwrap();
        let rewritten = VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 5);
        assert_eq!(rewritten.digest(), sha(b"other content, longer"));
        VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 5);
    }

    #[cfg(unix)]
    #[test]
    fn recorded_checksums_outlive_the_process_and_are_ignored_for_any_other_stamp() {
        let data = tempfile::tempdir().unwrap();
        let lease = crate::storage::DirectoryLease::acquire(data.path().join("data")).unwrap();
        let record = lease.root().join(checksums::FILENAME);
        let directory = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(directory.path())
            .unwrap()
            .join("weights.gguf");
        std::fs::write(&path, b"first content").unwrap();
        let entry = |field: &str| {
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
            assert_eq!(saved["schema_version"], 1);
            saved["files"][path.to_str().unwrap()][field].clone()
        };
        let open = |checksums: &FileChecksums| {
            VerifiedFile::open_remembered(&path, deadline(), Some(checksums)).unwrap()
        };

        let reads = full_reads();
        let first = open(&FileChecksums::open(lease.clone()));
        assert_eq!(full_reads(), reads + 1);
        assert_eq!(entry("sha256"), sha(b"first content"));
        assert_eq!(entry("bytes"), 13);
        assert_eq!(entry("device"), serde_json::Value::Null);

        forget_in_process(&path);
        let restarted = open(&FileChecksums::open(lease.clone()));
        assert_eq!(full_reads(), reads + 1);
        assert_eq!(restarted.digest(), first.digest());
        restarted.verify_current().unwrap();

        forget_in_process(&path);
        VerifiedFile::open(&path, deadline()).unwrap();
        assert_eq!(full_reads(), reads + 2);

        std::fs::write(&path, b"other content").unwrap();
        let changed = open(&FileChecksums::open(lease.clone()));
        assert_eq!(full_reads(), reads + 3);
        assert_eq!(changed.digest(), sha(b"other content"));
        assert_eq!(entry("sha256"), sha(b"other content"));

        let moved = path.with_file_name("moved.gguf");
        std::fs::hard_link(&path, &moved).unwrap();
        std::fs::remove_file(&moved).unwrap();
        forget_in_process(&path);
        let relinked = open(&FileChecksums::open(lease.clone()));
        assert_eq!(full_reads(), reads + 4);
        assert_eq!(relinked.digest(), changed.digest());

        forget_in_process(&path);
        let saved = std::fs::read_to_string(&record).unwrap();
        std::fs::write(
            &record,
            saved.replace(&sha(b"other content"), &"0".repeat(63)),
        )
        .unwrap();
        assert_eq!(
            open(&FileChecksums::open(lease.clone())).digest(),
            sha(b"other content")
        );
        assert_eq!(full_reads(), reads + 5);

        forget_in_process(&path);
        std::fs::write(&record, b"{not json").unwrap();
        open(&FileChecksums::open(lease));
        assert_eq!(full_reads(), reads + 6);
        assert_eq!(entry("sha256"), sha(b"other content"));
    }

    #[cfg(unix)]
    #[test]
    fn a_stamp_settles_only_after_the_tick_its_timestamps_were_taken_in() {
        use std::time::UNIX_EPOCH;
        let now = UNIX_EPOCH + Duration::new(2_000_000_000, 500_000_000);
        let stamp = |modified: (i64, i64), changed: (i64, i64)| stamp::Stamp {
            device: 1,
            inode: 2,
            bytes: 3,
            modified,
            changed,
        };
        let fresh = stamp((1_000, 7), (2_000_000_000, 495_000_000));
        assert_eq!(fresh.unsettled(now), Some(Duration::from_millis(15)));
        assert!(fresh.unsettled(now).unwrap() <= stamp::TICK);
        let old = stamp((1_000, 7), (2_000_000_000, 400_000_000));
        assert_eq!(old.unsettled(now), None);
        let whole_seconds = stamp((1_999_999_999, 0), (1_999_999_999, 0));
        assert_eq!(
            whole_seconds.unsettled(now),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            stamp((1_999_999_998, 0), (1_999_999_998, 0)).unsettled(now),
            None
        );
        let ahead = stamp((2_000_000_060, 1), (1_000, 7));
        assert!(ahead.unsettled(now).unwrap() > stamp::TICK);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_that_began_before_the_stamp_settled_is_not_remembered() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights.bin");
        std::fs::write(&path, b"first content").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let changed = metadata.modified().unwrap();
        remember_read(&path, &metadata, &metadata, changed, "early", None);
        assert_eq!(cached_digest(&metadata), None);
        remember_read(
            &path,
            &metadata,
            &metadata,
            changed + Duration::from_secs(1),
            "settled",
            None,
        );
        assert_eq!(cached_digest(&metadata).as_deref(), Some("settled"));
    }
}
