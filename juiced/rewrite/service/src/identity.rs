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
    Ok(VerifiedFile::open(path, deadline)?.digest)
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
