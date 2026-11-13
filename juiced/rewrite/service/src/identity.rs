use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
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
        Self::open_checked(path, deadline, || Ok(()))
    }

    pub(crate) fn open_checked(
        path: &Path,
        deadline: Instant,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Self> {
        check()?;
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
        Self::from_file_checked(&path, file, deadline, check)
    }

    pub(crate) fn from_file_checked(
        path: &Path,
        mut file: File,
        deadline: Instant,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Self> {
        check()?;
        let before = file.metadata()?;
        if !before.is_file() || before.file_type().is_symlink() {
            return Err(io::Error::other("Identity requires a regular file"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            if Instant::now() > deadline {
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
        #[cfg(unix)]
        if stamp::Stamp::of(&verified.file.metadata()?) == stamp::Stamp::of(&before) {
            stamp::remember(stamp::Stamp::of(&before), &verified.digest);
        }
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
    {
        let before = stamp::Stamp::of(&std::fs::metadata(std::fs::canonicalize(path)?)?);
        if let Some(digest) = stamp::cached(&before) {
            return Ok(digest);
        }
        let verified = VerifiedFile::open(path, deadline)?;
        if stamp::Stamp::of(&verified.file.metadata()?) == before {
            stamp::remember(before, &verified.digest);
        }
        Ok(verified.digest)
    }
    #[cfg(not(unix))]
    {
        Ok(VerifiedFile::open(path, deadline)?.digest)
    }
}

pub(crate) fn cached_digest(metadata: &std::fs::Metadata) -> Option<String> {
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

pub(crate) fn remember_digest(metadata: &std::fs::Metadata, digest: &str) {
    #[cfg(unix)]
    stamp::remember(stamp::Stamp::of(metadata), digest);
    #[cfg(not(unix))]
    let _ = (metadata, digest);
}

#[cfg(unix)]
mod stamp {
    use std::{
        collections::HashMap,
        os::unix::fs::MetadataExt,
        sync::{Mutex, OnceLock},
    };

    /// Large enough for the TheNoise bundle's ~30,000 files, so a launch does not rehash them.
    const ENTRIES: usize = 65536;

    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    pub(super) struct Stamp {
        device: u64,
        inode: u64,
        bytes: u64,
        modified: (i64, i64),
        changed: (i64, i64),
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

    #[test]
    fn repeated_digests_reuse_unchanged_stamps_and_rehash_rewritten_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights.bin");
        std::fs::write(&path, b"first content").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = file_digest(&path, deadline).unwrap();
        assert_eq!(first, format!("{:x}", Sha256::digest(b"first content")));
        assert_eq!(file_digest(&path, deadline).unwrap(), first);
        std::fs::write(&path, b"other content").unwrap();
        assert_eq!(
            file_digest(&path, deadline).unwrap(),
            format!("{:x}", Sha256::digest(b"other content"))
        );
        let verified = VerifiedFile::open(&path, deadline).unwrap();
        assert_eq!(file_digest(&path, deadline).unwrap(), verified.digest());
    }
}
