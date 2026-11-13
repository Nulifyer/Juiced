//! Durable record of in-flight acquisitions (library pulls and native `/pull` jobs) so they
//! survive a restart. Transfer bytes and range validators stay in the artifact store; this
//! journal only remembers which jobs belong to which pull and what to do when they finish.

use crate::storage::{DirectoryLease, LiveRoots};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub const FILENAME: &str = "acquisitions.json";
const MAXIMUM_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Document {
    schema_version: u32,
    #[serde(default)]
    pulls: BTreeMap<String, Value>,
    #[serde(default)]
    native: BTreeMap<String, Value>,
}

#[derive(Clone, Copy)]
pub enum Section {
    Pulls,
    Native,
}

pub struct Journal {
    root: DirectoryLease,
    scope: Arc<LiveRoots>,
    document: Mutex<Document>,
    failed: std::sync::atomic::AtomicBool,
}

impl Journal {
    /// Opens the journal; an unreadable or corrupt file starts empty and is kept aside so the
    /// service still starts.
    pub fn open(root: DirectoryLease, scope: Arc<LiveRoots>) -> Arc<Self> {
        let document = match root.read_bytes(FILENAME, MAXIMUM_BYTES) {
            Ok(bytes) => match serde_json::from_slice::<Document>(&bytes) {
                Ok(document) if document.schema_version == 1 => document,
                _ => {
                    if let Ok(path) = root.file_path(FILENAME) {
                        let _ = std::fs::rename(&path, path.with_extension("json.corrupt"));
                    }
                    Document::default()
                }
            },
            Err(_) => Document::default(),
        };
        Arc::new(Self {
            root,
            scope,
            document: Mutex::new(Document {
                schema_version: 1,
                ..document
            }),
            failed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn entries(&self, section: Section) -> Vec<(String, Value)> {
        let document = self
            .document
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let map = match section {
            Section::Pulls => &document.pulls,
            Section::Native => &document.native,
        };
        map.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub fn put(&self, section: Section, id: &str, value: Value) {
        self.change(section, |map| {
            if map.get(id) == Some(&value) {
                return false;
            }
            map.insert(id.to_owned(), value);
            true
        });
    }

    pub fn remove(&self, section: Section, id: &str) {
        self.change(section, |map| map.remove(id).is_some());
    }

    /// True after a write failed; later successful writes clear it.
    pub fn degraded(&self) -> bool {
        self.failed.load(std::sync::atomic::Ordering::Acquire)
    }

    fn change(&self, section: Section, edit: impl FnOnce(&mut BTreeMap<String, Value>) -> bool) {
        let mut document = self
            .document
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let map = match section {
            Section::Pulls => &mut document.pulls,
            Section::Native => &mut document.native,
        };
        if !edit(map) {
            return;
        }
        let written = self.scope.write_json(&self.root, FILENAME, &*document);
        self.failed
            .store(written.is_err(), std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entries_survive_reopen_and_corrupt_files_start_empty() {
        let directory = tempfile::tempdir().unwrap();
        let open = || {
            let lease = DirectoryLease::acquire(directory.path()).unwrap();
            let scope = LiveRoots::single(lease.clone()).unwrap();
            Journal::open(lease, scope)
        };
        let journal = open();
        journal.put(Section::Pulls, "a", json!({"repo": "x/y"}));
        journal.put(Section::Native, "model:m", json!({"status": "paused"}));
        journal.remove(Section::Pulls, "missing");
        assert!(!journal.degraded());
        drop(journal);
        let reopened = open();
        assert_eq!(
            reopened.entries(Section::Pulls),
            vec![("a".to_owned(), json!({"repo": "x/y"}))]
        );
        reopened.remove(Section::Native, "model:m");
        assert!(reopened.entries(Section::Native).is_empty());
        drop(reopened);
        std::fs::write(directory.path().join(FILENAME), b"{not json").unwrap();
        assert!(open().entries(Section::Pulls).is_empty());
        assert!(directory.path().join("acquisitions.json.corrupt").exists());
    }
}
