use crate::ApiError;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

pub const GIB: u64 = 1024 * 1024 * 1024;
const MAXIMUM_RESERVE_BYTES: u64 = 1024 * 1024 * GIB;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// Free space every download, install and snapshot write must leave behind.
    pub reserve_bytes: u64,
    /// Free space below which the Dashboard warns.
    pub low_space_bytes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            reserve_bytes: 2 * GIB,
            low_space_bytes: 20 * GIB,
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.reserve_bytes > MAXIMUM_RESERVE_BYTES
            || self.low_space_bytes > MAXIMUM_RESERVE_BYTES
        {
            return Err("storage reserve_bytes and low_space_bytes must be at most 1 PiB");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Volume {
    pub device: u64,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub read_only: bool,
}

#[cfg(unix)]
pub fn volume(path: &Path) -> Option<Volume> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let device = std::fs::metadata(path).ok()?.dev();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `name` is a valid NUL-terminated path and `stats` is a writable out-parameter.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    Some(Volume {
        device,
        total_bytes: (stats.f_blocks as u64).saturating_mul(block),
        available_bytes: (stats.f_bavail as u64).saturating_mul(block),
        read_only: stats.f_flag & libc::ST_RDONLY != 0,
    })
}

#[cfg(not(unix))]
pub fn volume(_path: &Path) -> Option<Volume> {
    None
}

/// The nearest existing ancestor, so a store that has not been created yet still reports the
/// volume it will land on.
pub fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|candidate| candidate.exists())
}

#[derive(Clone, Debug, Serialize)]
pub struct Shortfall {
    pub path: PathBuf,
    pub required_bytes: u64,
    pub available_bytes: u64,
    pub reserve_bytes: u64,
    pub shortfall_bytes: u64,
}

impl Shortfall {
    pub fn message(&self, action: &str) -> String {
        format!(
            "Not enough free space to {action}: it needs {} plus the {} reserve, but {} is free on {} ({} short). Free space under Engines & hardware > Storage, then retry.",
            human(self.required_bytes),
            human(self.reserve_bytes),
            human(self.available_bytes),
            self.path.display(),
            human(self.shortfall_bytes),
        )
    }

    pub fn into_error(self, action: &str) -> ApiError {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            self.message(action),
        )
    }
}

/// Checks that every volume touched by `needs` keeps `reserve` free after the writes land.
/// Needs on the same device add up; paths whose volume cannot be read are not refused.
pub fn require(needs: &[(&Path, u64)], reserve: u64) -> Result<(), Shortfall> {
    let mut devices: BTreeMap<u64, (PathBuf, u64, u64)> = BTreeMap::new();
    for (path, bytes) in needs {
        let Some(existing) = existing_ancestor(path) else {
            continue;
        };
        let Some(volume) = volume(existing) else {
            continue;
        };
        let entry = devices
            .entry(volume.device)
            .or_insert_with(|| (existing.to_owned(), 0, volume.available_bytes));
        entry.1 = entry.1.saturating_add(*bytes);
    }
    for (path, required, available) in devices.into_values() {
        let wanted = required.saturating_add(reserve);
        if required > 0 && wanted > available {
            return Err(Shortfall {
                path,
                required_bytes: required,
                available_bytes: available,
                reserve_bytes: reserve,
                shortfall_bytes: wanted - available,
            });
        }
    }
    Ok(())
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct TreeSize {
    pub bytes: u64,
    pub files: u64,
    pub truncated: bool,
}

/// Sums regular-file sizes under `path` without following symlinks. Stops at `maximum_entries`
/// or `deadline` and reports a lower bound.
pub fn tree_size(path: &Path, maximum_entries: u64, deadline: Instant) -> TreeSize {
    let mut size = TreeSize::default();
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return size;
    };
    if metadata.is_file() {
        size.bytes = metadata.len();
        size.files = 1;
        return size;
    }
    if !metadata.is_dir() {
        return size;
    }
    let mut pending = vec![path.to_owned()];
    let mut seen = 0u64;
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > maximum_entries || (seen % 256 == 0 && Instant::now() > deadline) {
                size.truncated = true;
                return size;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && let Ok(metadata) = entry.metadata()
            {
                size.bytes = size.bytes.saturating_add(metadata.len());
                size.files += 1;
            }
        }
    }
    size
}

/// Restores owner write permission below `path` so a tree that holds published read-only
/// payloads can be removed. Symlinks are never followed.
pub fn make_writable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mode = metadata.permissions().mode();
        if mode & 0o700 != 0o700 && metadata.is_dir() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700))?;
        } else if mode & 0o200 == 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o200))?;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                make_writable(&entry?.path())?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        make_writable(path)?;
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn require_adds_needs_on_one_device_and_reports_the_shortfall() {
        let directory = tempfile::tempdir().unwrap();
        let available = volume(directory.path()).unwrap().available_bytes;
        assert!(require(&[(directory.path(), 1)], 0).is_ok());
        let short = require(
            &[
                (directory.path(), available / 2 + 1),
                (&directory.path().join("missing/child"), available / 2 + 1),
            ],
            0,
        )
        .unwrap_err();
        assert!(short.shortfall_bytes >= 1);
        assert_eq!(short.required_bytes, (available / 2 + 1) * 2);
        let error = require(&[(directory.path(), 1)], available + GIB)
            .unwrap_err()
            .into_error("download this model");
        assert_eq!(error.status, StatusCode::INSUFFICIENT_STORAGE);
        assert!(error.message.contains("short"), "{}", error.message);
    }

    #[test]
    fn tree_size_counts_files_without_following_links_and_removes_read_only_trees() {
        let directory = tempfile::tempdir().unwrap();
        let tree = directory.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::write(tree.join("a"), [0; 10]).unwrap();
        std::fs::write(tree.join("nested/b"), [0; 5]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::os::unix::fs::symlink(directory.path(), tree.join("loop")).unwrap();
            std::fs::set_permissions(tree.join("nested"), std::fs::Permissions::from_mode(0o500))
                .unwrap();
        }
        let size = tree_size(&tree, 100, Instant::now() + Duration::from_secs(5));
        assert_eq!((size.bytes, size.files, size.truncated), (15, 2, false));
        assert!(tree_size(&tree, 1, Instant::now() + Duration::from_secs(5)).truncated);
        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
        assert!(directory.path().exists());
    }
}
