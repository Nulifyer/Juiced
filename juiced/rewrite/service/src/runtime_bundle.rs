use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};

pub(crate) const MAX_FILES: usize = 256;
pub(crate) const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_ENTRIES: usize = 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileRole {
    Executable,
    Library,
    Resource,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    pub path: PathBuf,
    pub sha256: String,
    pub role: FileRole,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSpec {
    pub schema_version: u32,
    pub implementation_id: String,
    pub root: PathBuf,
    pub entrypoint: PathBuf,
    pub library_directories: Vec<PathBuf>,
    pub files: Vec<BundleFile>,
}

pub struct VerifiedBundle {
    pub executable: PathBuf,
    pub root: PathBuf,
    pub files: BTreeMap<PathBuf, String>,
    pub digest: String,
    library_path: OsString,
    spec: BundleSpec,
}

fn invalid() -> String {
    "The runtime bundle needs a supported version, exact implementation, bounded relative paths, unique files and SHA256 identities.".into()
}

fn relative(path: &Path, allow_root: bool) -> bool {
    (allow_root && path == Path::new("."))
        || (!path.as_os_str().is_empty()
            && path.as_os_str().len() <= 4096
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && !path.to_string_lossy().chars().any(char::is_control))
}

impl BundleSpec {
    pub fn validate(&self, provider: crate::config::Provider) -> Result<(), String> {
        let expected = provider.implementation_id().ok_or_else(invalid)?;
        if self.schema_version != 1
            || self.implementation_id != expected
            || self.root.as_os_str().is_empty()
            || !relative(&self.entrypoint, false)
            || self.files.is_empty()
            || self.files.len() > MAX_FILES
            || self.library_directories.len() > 16
            || self
                .library_directories
                .iter()
                .any(|path| !relative(path, true))
        {
            return Err(invalid());
        }
        let mut paths = BTreeSet::new();
        for file in &self.files {
            if !relative(&file.path, false)
                || !paths.insert(&file.path)
                || file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid());
            }
        }
        if !self
            .files
            .iter()
            .any(|file| file.path == self.entrypoint && file.role == FileRole::Executable)
            || self
                .library_directories
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.library_directories.len()
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn verify(&self, deadline: Instant) -> Result<VerifiedBundle, String> {
        let provider = match self.implementation_id.as_str() {
            "llama-cpp" => crate::config::Provider::LlamaCpp,
            "whisper-cpp" => crate::config::Provider::WhisperCpp,
            _ => return Err(invalid()),
        };
        self.validate(provider)?;
        let refused = || {
            "The runtime bundle is incomplete, changed, linked or outside its inspection bounds."
                .to_owned()
        };
        let root = fs::canonicalize(&self.root).map_err(|_| refused())?;
        if !fs::symlink_metadata(&self.root)
            .map_err(|_| refused())?
            .is_dir()
        {
            return Err(refused());
        }
        let expected: BTreeSet<_> = self.files.iter().map(|file| file.path.clone()).collect();
        let mut found = BTreeSet::new();
        let mut pending = vec![root.clone()];
        let mut entries = 0;
        let mut bytes = 0u64;
        while let Some(directory) = pending.pop() {
            for item in fs::read_dir(directory).map_err(|_| refused())? {
                entries += 1;
                if entries > MAX_ENTRIES || Instant::now() >= deadline {
                    return Err(refused());
                }
                let item = item.map_err(|_| refused())?;
                let metadata = item.path().symlink_metadata().map_err(|_| refused())?;
                if metadata.is_dir() {
                    pending.push(item.path());
                } else if metadata.is_file() {
                    bytes = bytes.checked_add(metadata.len()).ok_or_else(refused)?;
                    if metadata.len() > MAX_FILE_BYTES || bytes > MAX_TOTAL_BYTES {
                        return Err(refused());
                    }
                    found.insert(
                        item.path()
                            .strip_prefix(&root)
                            .map_err(|_| refused())?
                            .to_owned(),
                    );
                } else {
                    return Err(refused());
                }
            }
        }
        if found != expected {
            return Err(refused());
        }
        let mut digest = Sha256::new();
        digest.update(b"juiced-runtime-bundle-v1\0");
        digest.update(
            serde_json::to_vec(&(
                &self.implementation_id,
                &self.entrypoint,
                &self.library_directories,
            ))
            .map_err(|_| refused())?,
        );
        let mut files: Vec<_> = self.files.iter().collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut verified_files = BTreeMap::new();
        for file in files {
            let path = root.join(&file.path);
            if crate::identity::file_digest(&path, deadline).map_err(|_| refused())? != file.sha256
            {
                return Err(refused());
            }
            digest.update(
                serde_json::to_vec(&(&file.path, &file.sha256, file.role))
                    .map_err(|_| refused())?,
            );
            verified_files.insert(path, file.sha256.clone());
        }
        let directories = self
            .library_directories
            .iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>();
        if directories.iter().any(|path| !path.is_dir()) {
            return Err(refused());
        }
        let library_path = std::env::join_paths(directories).map_err(|_| refused())?;
        Ok(VerifiedBundle {
            executable: root.join(&self.entrypoint),
            root,
            files: verified_files,
            digest: format!("{:x}", digest.finalize()),
            library_path,
            spec: self.clone(),
        })
    }
}

impl VerifiedBundle {
    pub fn verify_fresh(&self, deadline: Instant) -> Result<(), crate::runtime_identity::Refusal> {
        self.spec
            .verify(deadline)
            .map(|_| ())
            .map_err(|_| crate::runtime_identity::Refusal {
                status: crate::runtime_identity::Status::Unqualified,
                code: "runtime_bundle_changed",
                message: "The runtime bundle changed or its complete file identity is unavailable.",
            })
    }
    pub(crate) fn configure_environment(
        &self,
        environment: &mut BTreeMap<OsString, OsString>,
    ) -> std::io::Result<()> {
        for name in ["LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES"] {
            environment.remove(std::ffi::OsStr::new(name));
        }
        #[cfg(target_os = "linux")]
        environment.insert("LD_LIBRARY_PATH".into(), self.library_path.clone());
        #[cfg(target_os = "macos")]
        environment.insert("DYLD_LIBRARY_PATH".into(), self.library_path.clone());
        #[cfg(windows)]
        {
            let inherited = environment
                .keys()
                .find(|name| name.to_string_lossy().eq_ignore_ascii_case("PATH"))
                .cloned();
            let value = inherited
                .and_then(|key| environment.remove(&key))
                .unwrap_or_default();
            let mut paths: Vec<_> = std::env::split_paths(&self.library_path).collect();
            paths.extend(std::env::split_paths(&value));
            let path = std::env::join_paths(paths).map_err(std::io::Error::other)?;
            environment.insert("PATH".into(), path);
        }
        Ok(())
    }

    pub fn verify_observation(
        &self,
        observation: &crate::runtime_identity::Observation,
    ) -> Result<(), crate::runtime_identity::Refusal> {
        crate::runtime_identity::verify_bundle_files(observation, &self.root, &self.files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bundle(root: &Path) -> BundleSpec {
        fs::write(root.join("engine"), b"exact engine").unwrap();
        fs::write(root.join("runtime.so"), b"exact library").unwrap();
        BundleSpec {
            schema_version: 1,
            implementation_id: "whisper-cpp".into(),
            root: root.to_owned(),
            entrypoint: "engine".into(),
            library_directories: vec![".".into()],
            files: vec![
                BundleFile {
                    path: "engine".into(),
                    sha256: format!("{:x}", Sha256::digest(b"exact engine")),
                    role: FileRole::Executable,
                },
                BundleFile {
                    path: "runtime.so".into(),
                    sha256: format!("{:x}", Sha256::digest(b"exact library")),
                    role: FileRole::Library,
                },
            ],
        }
    }
    fn deadline() -> Instant {
        Instant::now() + std::time::Duration::from_secs(2)
    }

    #[test]
    fn verified_package_identity_is_order_independent_and_detects_changes() {
        let directory = tempfile::tempdir().unwrap();
        let mut spec = bundle(directory.path());
        spec.validate(crate::config::Provider::WhisperCpp).unwrap();
        let verified = spec.verify(deadline()).unwrap();
        spec.files.reverse();
        assert_eq!(verified.digest, spec.verify(deadline()).unwrap().digest);
        fs::write(directory.path().join("runtime.so"), b"other library").unwrap();
        assert!(spec.verify(deadline()).is_err());
    }

    #[test]
    fn incomplete_unlisted_expired_and_mismatched_bundles_reject() {
        let directory = tempfile::tempdir().unwrap();
        let spec = bundle(directory.path());
        assert!(spec.validate(crate::config::Provider::LlamaCpp).is_err());
        assert!(spec.verify(Instant::now()).is_err());
        fs::write(directory.path().join("unlisted.so"), b"inject").unwrap();
        assert!(spec.verify(deadline()).is_err());
        fs::remove_file(directory.path().join("unlisted.so")).unwrap();
        fs::remove_file(directory.path().join("runtime.so")).unwrap();
        assert!(spec.verify(deadline()).is_err());
    }

    #[test]
    fn invalid_paths_duplicates_and_hashes_reject() {
        let directory = tempfile::tempdir().unwrap();
        let original = bundle(directory.path());
        for path in ["../escape", "/outside", "", "./engine"] {
            let mut spec = original.clone();
            spec.files[0].path = path.into();
            assert!(spec.validate(crate::config::Provider::WhisperCpp).is_err());
        }
        let mut spec = original.clone();
        spec.files.push(spec.files[0].clone());
        assert!(spec.validate(crate::config::Provider::WhisperCpp).is_err());
        let mut spec = original;
        spec.files[0].sha256 = "F".repeat(64);
        assert!(spec.validate(crate::config::Provider::WhisperCpp).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn package_links_reject_without_reading_their_target() {
        let directory = tempfile::tempdir().unwrap();
        let spec = bundle(directory.path());
        fs::remove_file(directory.path().join("runtime.so")).unwrap();
        std::os::unix::fs::symlink(
            directory.path().join("engine"),
            directory.path().join("runtime.so"),
        )
        .unwrap();
        assert!(spec.verify(deadline()).is_err());
    }
}
