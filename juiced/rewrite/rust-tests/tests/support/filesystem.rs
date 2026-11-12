use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Eq, PartialEq)]
pub enum Entry {
    Directory,
    File { bytes: u64, sha256: String },
}
pub fn digest(path: &Path) -> String {
    let mut file = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    format!("{:x}", hash.finalize())
}
pub fn tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Entry>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if metadata.is_dir() {
                entries.insert(relative, Entry::Directory);
                visit(root, &path, entries);
            } else {
                assert!(metadata.is_file());
                entries.insert(
                    relative,
                    Entry::File {
                        bytes: metadata.len(),
                        sha256: digest(&path),
                    },
                );
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[cfg(unix)]
pub struct ReplacedRoot {
    path: PathBuf,
    original: PathBuf,
    _holding: tempfile::TempDir,
    foreign: BTreeMap<PathBuf, Entry>,
}
#[cfg(unix)]
impl ReplacedRoot {
    pub fn replace(path: &Path) -> Self {
        fn copy(source: &Path, target: &Path) {
            std::fs::create_dir(target).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                let source = entry.path();
                let destination = target.join(entry.file_name());
                let metadata = std::fs::symlink_metadata(&source).unwrap();
                assert!(!metadata.file_type().is_symlink());
                if metadata.is_dir() {
                    copy(&source, &destination);
                } else {
                    assert!(metadata.is_file());
                    std::fs::copy(source, destination).unwrap();
                }
            }
        }
        let holding = tempfile::tempdir().unwrap();
        let original = holding.path().join("original");
        std::fs::rename(path, &original).unwrap();
        copy(&original, path);
        std::fs::write(
            path.join("foreign-marker"),
            b"replacement tree is not owned",
        )
        .unwrap();
        Self {
            path: path.to_owned(),
            original,
            _holding: holding,
            foreign: tree(path),
        }
    }
    pub fn original(&self) -> &Path {
        &self.original
    }
    pub fn assert_foreign_unchanged(&self) {
        assert_eq!(tree(&self.path), self.foreign);
    }
    pub fn restore(self) {
        self.assert_foreign_unchanged();
        std::fs::remove_dir_all(&self.path).unwrap();
        std::fs::rename(&self.original, &self.path).unwrap();
    }
}
