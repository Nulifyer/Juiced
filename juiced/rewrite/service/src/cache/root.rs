use crate::storage::CONTROL_FILE;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

// Native Juiced keeps engine binaries in `<data>/cache`, so the rewrite's snapshots live in
// their own directory that can be a dedicated volume.
pub const STORE_ROOT: &str = "kv-cache";
const LEGACY_ROOT: &str = "cache";

pub fn store_root(parent: &Path) -> PathBuf {
    parent.join(STORE_ROOT)
}

#[derive(Debug, Eq, PartialEq)]
pub enum Preparation {
    Ready,
    Moved,
    LegacyRetained { files: usize, bytes: u64 },
}

impl Preparation {
    pub fn notice(&self, parent: &Path) -> Option<String> {
        let legacy = parent.join(LEGACY_ROOT);
        let root = store_root(parent);
        match self {
            Self::Ready => None,
            Self::Moved => Some(format!(
                "Moved session snapshots from {} to {}",
                legacy.display(),
                root.display()
            )),
            Self::LegacyRetained { files, bytes } => Some(format!(
                "{files} earlier session snapshot files ({bytes} bytes) remain in {}, which also holds other files. They are no longer used; delete its kv-*.bin, snapshots.json and recovery files to reclaim the space.",
                legacy.display()
            )),
        }
    }
}

// An empty directory is a freshly mounted volume and is claimed. A directory with files but no
// control file belongs to something else and is refused rather than mixed into.
pub fn prepare(parent: &Path) -> io::Result<Preparation> {
    let root = store_root(parent);
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {
            if !root.join(CONTROL_FILE).exists() && fs::read_dir(&root)?.next().is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} holds files that are not a Juiced snapshot store. Mount an empty volume there or move the files away.",
                        root.display()
                    ),
                ));
            }
            return Ok(Preparation::Ready);
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a directory.", root.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let legacy = parent.join(LEGACY_ROOT);
    let Ok(metadata) = fs::symlink_metadata(&legacy) else {
        return Ok(Preparation::Ready);
    };
    if !metadata.is_dir()
        || !legacy.join(CONTROL_FILE).is_file()
        || !legacy.join(super::MANIFEST).is_file()
    {
        return Ok(Preparation::Ready);
    }
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut foreign = false;
    for entry in fs::read_dir(&legacy)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        let metadata = entry.metadata()?;
        if metadata.is_file() && store_file(name) {
            if name != CONTROL_FILE {
                files += 1;
                bytes = bytes.saturating_add(metadata.len());
            }
        } else {
            foreign = true;
        }
    }
    if foreign {
        return Ok(Preparation::LegacyRetained { files, bytes });
    }
    fs::rename(&legacy, &root)?;
    Ok(Preparation::Moved)
}

fn store_file(name: &str) -> bool {
    name == CONTROL_FILE
        || name == super::MANIFEST
        || name.starts_with(".recovery-")
        || name.starts_with(".reader-recovery-")
        || name
            .strip_prefix("kv-")
            .and_then(|name| name.strip_suffix(".bin"))
            .is_some_and(super::writer::opaque_valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(parent: &Path) -> PathBuf {
        let legacy = parent.join(LEGACY_ROOT);
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join(CONTROL_FILE), b"").unwrap();
        fs::write(legacy.join(super::super::MANIFEST), b"{}").unwrap();
        fs::write(legacy.join(format!("kv-{}.bin", "a".repeat(32))), b"state").unwrap();
        legacy
    }

    #[test]
    fn missing_and_empty_roots_are_ready_and_foreign_content_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(prepare(directory.path()).unwrap(), Preparation::Ready);
        let root = store_root(directory.path());
        fs::create_dir(&root).unwrap();
        assert_eq!(prepare(directory.path()).unwrap(), Preparation::Ready);
        fs::write(root.join("unrelated"), b"keep").unwrap();
        assert_eq!(
            prepare(directory.path()).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(root.join("unrelated")).unwrap(), b"keep");
        fs::write(root.join(CONTROL_FILE), b"").unwrap();
        assert_eq!(prepare(directory.path()).unwrap(), Preparation::Ready);
    }

    #[test]
    fn a_store_only_legacy_root_moves_and_a_shared_one_is_left_in_place() {
        let directory = tempfile::tempdir().unwrap();
        legacy(directory.path());
        assert_eq!(prepare(directory.path()).unwrap(), Preparation::Moved);
        assert!(!directory.path().join(LEGACY_ROOT).exists());
        assert!(
            store_root(directory.path())
                .join(format!("kv-{}.bin", "a".repeat(32)))
                .is_file()
        );

        let shared = tempfile::tempdir().unwrap();
        let old = legacy(shared.path());
        fs::create_dir(old.join("bin")).unwrap();
        assert_eq!(
            prepare(shared.path()).unwrap(),
            Preparation::LegacyRetained { files: 2, bytes: 7 }
        );
        assert!(old.join("bin").is_dir());
        assert!(!store_root(shared.path()).exists());

        let native = tempfile::tempdir().unwrap();
        fs::create_dir(native.path().join(LEGACY_ROOT)).unwrap();
        fs::write(native.path().join(LEGACY_ROOT).join("engine"), b"x").unwrap();
        assert_eq!(prepare(native.path()).unwrap(), Preparation::Ready);
    }
}
