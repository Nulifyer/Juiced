use crate::runtime_bundle::BundleSpec;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) mod memory;

#[cfg(unix)]
mod native;
#[cfg(unix)]
pub(crate) use native::managed::{Lease, Store};
#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
pub(crate) use unsupported::{Lease, Store};

pub(crate) fn valid_id(id: &str) -> bool {
    id.strip_prefix("package-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub builder_id: String,
    pub source_revision: String,
    pub build_revision: String,
    pub declared_licenses: Vec<String>,
    pub declared_sbom: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseSource>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSource {
    pub engine: String,
    pub backend: String,
    pub gpu_target: Option<String>,
    pub repository: String,
    pub tag: String,
    pub asset: String,
    pub archive_sha256: String,
    pub archive_bytes: u64,
    pub digest_source: String,
}
impl ReleaseSource {
    pub(crate) fn valid(&self) -> bool {
        let text = |value: &str| {
            !value.is_empty()
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-+/@:".contains(&byte))
        };
        text(&self.engine)
            && text(&self.backend)
            && self.gpu_target.as_deref().is_none_or(text)
            && text(&self.repository)
            && text(&self.tag)
            && text(&self.asset)
            && text(&self.digest_source)
            && self.archive_sha256.len() == 64
            && self
                .archive_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredTarget {
    pub operating_system: String,
    pub architecture: String,
    pub abi: String,
    pub compute_paths: Vec<String>,
}
#[cfg(unix)]
impl DeclaredTarget {
    pub(crate) fn matches_service_target(&self) -> bool {
        let os = match self.operating_system.as_str() {
            "darwin" => "macos",
            "win32" => "windows",
            other => other,
        };
        let architecture = match self.architecture.as_str() {
            "x64" | "amd64" => "x86_64",
            "arm64" => "aarch64",
            "ia32" | "i386" | "i686" => "x86",
            other => other,
        };
        os == std::env::consts::OS && architecture == std::env::consts::ARCH
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemberBytes {
    pub path: PathBuf,
    pub bytes: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub schema_version: u32,
    pub bundle: BundleSpec,
    pub provenance: Provenance,
    pub declared_target: DeclaredTarget,
    pub files: Vec<MemberBytes>,
}

#[derive(Clone, Copy)]
pub struct Error {
    reason: &'static str,
}
impl Error {
    pub(crate) fn new(reason: &'static str) -> Self {
        Self { reason }
    }
    pub fn reason(&self) -> &'static str {
        self.reason
    }
    pub fn code(&self) -> &'static str {
        "runtime_package_recovery_required"
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: reason={}; Preserve the source, package manifest and incomplete payload.",
            self.code(),
            self.reason
        )
    }
}
impl std::error::Error for Error {}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimePackageRecoveryError")
            .field("code", &self.code())
            .field("reason", &self.reason)
            .finish()
    }
}
type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Serialize)]
pub struct ImportReceipt {
    pub schema_version: u32,
    pub package: String,
    pub installed: bool,
    pub newly_installed: bool,
    pub reserved_bytes: u64,
    pub reused_files: usize,
    pub bundle_digest: String,
    pub bundle: BundleSpec,
    pub compatibility: &'static str,
    pub scope: &'static str,
}

pub fn import_file(
    configuration: PathBuf,
    input: PathBuf,
    cancelled: Arc<AtomicBool>,
) -> Result<ImportReceipt> {
    #[cfg(unix)]
    {
        native::import_file(configuration, input, cancelled)
    }
    #[cfg(not(unix))]
    {
        let _ = (configuration, input, cancelled);
        Err(Error::new("platform_unsupported"))
    }
}

pub(crate) fn import_online(
    slot: &std::sync::OnceLock<Arc<Store>>,
    root: crate::storage::DirectoryLease,
    scope: Arc<crate::storage::LiveRoots>,
    input: PathBuf,
    expected_generation: u64,
    cancelled: Arc<AtomicBool>,
    control: Arc<memory::Controller>,
) -> Result<ImportReceipt> {
    #[cfg(unix)]
    {
        native::import_online(
            slot,
            root,
            scope,
            input,
            expected_generation,
            cancelled,
            control,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (
            slot,
            root,
            scope,
            input,
            expected_generation,
            cancelled,
            control,
        );
        Err(Error::new("platform_unsupported"))
    }
}
