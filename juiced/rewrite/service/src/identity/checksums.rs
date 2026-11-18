//! Digests of model files already read in full, kept in the data folder so a restart does
//! not read them again. An entry is trusted only while the file at its path still has the
//! recorded inode, size, modification time and change time.

use crate::storage::DirectoryLease;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::path::Path;
use std::{collections::BTreeMap, sync::Mutex};

pub const FILENAME: &str = "file-checksums.json";
const MAXIMUM_BYTES: u64 = 4 * 1024 * 1024;
const ENTRIES: usize = 4096;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(unix), allow(dead_code))]
struct Entry {
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    sha256: String,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(not(unix), allow(dead_code))]
struct Document {
    schema_version: u32,
    files: BTreeMap<String, Entry>,
}

#[cfg_attr(not(unix), allow(dead_code))]
pub struct FileChecksums {
    root: DirectoryLease,
    files: Mutex<BTreeMap<String, Entry>>,
}

impl FileChecksums {
    /// An unreadable or unrecognised file starts empty; the next full read replaces it.
    pub fn open(root: DirectoryLease) -> Self {
        let files = root
            .read_bytes(FILENAME, MAXIMUM_BYTES)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Document>(&bytes).ok())
            .filter(|document| document.schema_version == 1)
            .map(|document| document.files)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, entry)| {
                entry.sha256.len() == 64
                    && entry
                        .sha256
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
            .take(ENTRIES)
            .collect();
        Self {
            root,
            files: Mutex::new(files),
        }
    }

    #[cfg(unix)]
    pub(super) fn lookup(&self, path: &Path, stamp: &super::stamp::Stamp) -> Option<String> {
        let files = self.files.lock().ok()?;
        let entry = files.get(path.to_str()?)?;
        (*entry == Entry::of(stamp, &entry.sha256)).then(|| entry.sha256.clone())
    }

    #[cfg(unix)]
    pub(super) fn record(&self, path: &Path, stamp: &super::stamp::Stamp, digest: &str) {
        let (Some(path), Ok(mut files)) = (path.to_str(), self.files.lock()) else {
            return;
        };
        let entry = Entry::of(stamp, digest);
        if files.get(path) == Some(&entry) {
            return;
        }
        if files.len() >= ENTRIES && !files.contains_key(path) {
            files.retain(|path, _| {
                std::fs::symlink_metadata(path).map_or_else(
                    |error| error.kind() != std::io::ErrorKind::NotFound,
                    |_| true,
                )
            });
            if files.len() >= ENTRIES {
                files.clear();
            }
        }
        files.insert(path.to_owned(), entry);
        let _ = self.root.write_json(
            FILENAME,
            &Document {
                schema_version: 1,
                files: files.clone(),
            },
        );
    }
}

#[cfg(unix)]
impl Entry {
    /// The device number is left out: it can change when a volume is mounted again, and the
    /// path already says which filesystem the inode belongs to.
    fn of(stamp: &super::stamp::Stamp, digest: &str) -> Self {
        Self {
            inode: stamp.inode,
            bytes: stamp.bytes,
            modified: stamp.modified,
            changed: stamp.changed,
            sha256: digest.to_owned(),
        }
    }
}
