use crate::hardware::{GpuVendor, HostReport, NpuVendor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod llm;
mod media;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    TarGz,
    TarXz,
    Zip,
}

/// Engine metadata lives in `catalog/llm.rs` (text engines) and `catalog/media.rs` (audio,
/// image and 3D engines) as `EngineInfo` rows; each variant has exactly one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Engine {
    LlamaCpp,
    FastFlowLm,
    Vllm,
    Ds4,
    OnnxRuntime,
    // Media engines (catalog/media.rs).
    WhisperCpp,
    StableDiffusionCpp,
    Kokoros,
    Parakeet,
    Moonshine,
    Asr4all,
}

#[derive(Debug)]
pub struct EngineInfo {
    pub engine: Engine,
    /// The public engine name, used in API bodies and package provenance.
    pub name: &'static str,
    /// Other spellings `Engine::parse` accepts, including the runtime implementation id.
    pub aliases: &'static [&'static str],
    /// The native recipe whose support rows describe this engine's backends.
    pub recipe: &'static str,
    pub provider: crate::config::Provider,
    pub entrypoint: &'static str,
    /// The model format alone selects the engine and its only native support row is
    /// experimental, so no core backend exists that automatic selection could prefer.
    pub accepts_experimental: bool,
    pub licenses: &'static [&'static str],
}

impl Engine {
    pub fn all() -> impl Iterator<Item = Self> {
        llm::ENGINES
            .iter()
            .chain(media::ENGINES)
            .map(|info| info.engine)
    }
    pub fn info(self) -> &'static EngineInfo {
        llm::ENGINES
            .iter()
            .chain(media::ENGINES)
            .find(|info| info.engine == self)
            .expect("every engine has a catalog row")
    }
    pub fn parse(value: &str) -> Option<Self> {
        llm::ENGINES
            .iter()
            .chain(media::ENGINES)
            .find(|info| info.name == value || info.aliases.contains(&value))
            .map(|info| info.engine)
    }
    pub fn name(self) -> &'static str {
        self.info().name
    }
    pub fn recipe(self) -> &'static str {
        self.info().recipe
    }
    pub fn provider(self) -> crate::config::Provider {
        self.info().provider
    }
    pub fn entrypoint(self) -> &'static str {
        self.info().entrypoint
    }
    pub fn accepts_experimental(self) -> bool {
        self.info().accepts_experimental
    }
}

impl Serialize for Engine {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Engine {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name)
            .filter(|engine| engine.name() == name)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown engine {name}")))
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Build {
    pub engine: Engine,
    pub backend: &'static str,
    pub os: &'static str,
    pub architecture: &'static str,
    pub gpu_target: Option<&'static str>,
    pub repository: &'static str,
    pub tag: &'static str,
    pub asset: &'static str,
    pub archive_bytes: u64,
    pub sha256: Option<&'static str>,
    pub format: ArchiveFormat,
    pub runtime: Option<&'static Runtime>,
    /// A Python engine installs as an environment rather than a runtime package.
    pub environment: Option<&'static Environment>,
}

/// A Python engine environment. It is installed into the engine-environments store and
/// started through its own interpreter.
#[derive(Debug, Serialize)]
pub struct Environment {
    /// The engine version the environment provides, for display.
    pub version: &'static str,
    pub source: EnvironmentSource,
    /// Disk space the installed environment needs, checked before anything downloads.
    pub installed_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EnvironmentSource {
    /// The build archive is `uv`. It installs the pinned CPython build and then the
    /// hash-locked requirements, with PyTorch wheels from `torch_backend`.
    Uv {
        python: &'static str,
        torch_backend: &'static str,
        #[serde(skip)]
        lock: &'static str,
        lock_sha256: &'static str,
    },
    /// The release publishes a relocatable environment as a `.tar.gz` split into these
    /// parts. The build asset is the first part.
    Parts { parts: &'static [Part] },
}

#[derive(Debug, Serialize)]
pub struct Part {
    pub asset: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

impl Environment {
    /// Identifies the exact pinned inputs, so a changed pin is never mistaken for the
    /// installed environment.
    pub fn pin(&self) -> String {
        match &self.source {
            EnvironmentSource::Uv {
                python,
                torch_backend,
                lock_sha256,
                ..
            } => format!("uv:{python}:{torch_backend}:{lock_sha256}"),
            EnvironmentSource::Parts { parts } => format!(
                "parts:{}",
                parts
                    .iter()
                    .map(|part| part.sha256)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

/// A vendor runtime the engine archive links against but does not ship. It is extracted
/// into `directory` inside the engine bundle so the package stays self-contained.
#[derive(Debug, Serialize)]
pub struct Runtime {
    pub name: &'static str,
    pub version: &'static str,
    pub gpu_target: &'static str,
    pub path: &'static str,
    pub asset: &'static str,
    pub archive_bytes: u64,
    pub sha256: &'static str,
    pub format: ArchiveFormat,
    pub directory: &'static str,
    #[serde(skip)]
    pub members: &'static [&'static str],
}

impl Runtime {
    pub fn download_url(&self, base: &str) -> String {
        format!(
            "{}/{}/{}",
            base.trim_end_matches('/'),
            self.path,
            self.asset
        )
    }
    pub fn keeps(&self, path: &std::path::Path) -> bool {
        path.to_str()
            .is_some_and(|path| self.members.iter().any(|member| path.starts_with(member)))
    }
}

impl Build {
    pub fn download_url(&self, base: &str) -> String {
        format!(
            "{}/{}/releases/download/{}/{}",
            base.trim_end_matches('/'),
            self.repository,
            self.tag,
            self.asset
        )
    }
    pub fn release_api_url(&self, base: &str) -> String {
        format!(
            "{}/repos/{}/releases/tags/{}",
            base.trim_end_matches('/'),
            self.repository,
            self.tag
        )
    }
}

pub(super) const fn linux(
    engine: Engine,
    backend: &'static str,
    gpu_target: Option<&'static str>,
    release: (&'static str, &'static str),
    archive: (&'static str, u64, &'static str),
    format: ArchiveFormat,
) -> Build {
    let (asset, archive_bytes, sha256) = archive;
    Build {
        engine,
        backend,
        os: "linux",
        architecture: "x86_64",
        gpu_target,
        repository: release.0,
        tag: release.1,
        asset,
        archive_bytes,
        sha256: Some(sha256),
        format,
        runtime: None,
        environment: None,
    }
}

pub(super) const fn with_environment(build: Build, environment: &'static Environment) -> Build {
    Build {
        environment: Some(environment),
        ..build
    }
}

pub(super) const fn arm64(build: Build) -> Build {
    Build {
        architecture: "aarch64",
        ..build
    }
}

/// The native recipe whose support rows describe a backend. HRX is a llama.cpp build with
/// its own native recipe.
pub fn recipe_of(engine: Engine, backend: &str) -> &'static str {
    match (engine, backend) {
        (Engine::LlamaCpp, "hrx") => "llamacpp-hrx",
        _ => engine.recipe(),
    }
}

/// A support row for a backend Juiced adds beyond the native descriptors.
#[derive(Debug)]
pub struct SupportRow {
    pub recipe: &'static str,
    pub backend: &'static str,
    pub tier: &'static str,
    pub format: &'static str,
    pub os: &'static [&'static str],
    pub device_declaration: &'static str,
}

pub const SUPPLEMENTAL_SUPPORT_ROWS: &[SupportRow] = llm::SUPPLEMENTAL_SUPPORT_ROWS;

pub(super) const fn with_runtime(build: Build, runtime: &'static Runtime) -> Build {
    Build {
        runtime: Some(runtime),
        ..build
    }
}

pub const THEROCK_HOST: &str = "https://stable.repo.amd.com";

/// The shared libraries `libggml-hip.so` of the TheRock-linked llama.cpp and sd.cpp builds
/// needs (transitive `DT_NEEDED`), the rocBLAS/hipBLASLt kernel data, and the device-code
/// archives rocBLAS and rocSOLVER resolve at `../.kpack/` beside their library directory.
const THEROCK_MEMBERS: &[&str] = &[
    ".info/version",
    ".kpack/blas_lib_",
    ".kpack/solver_lib_",
    "lib/libamd_comgr.so",
    "lib/libamdhip64.so",
    "lib/libhipblas.so",
    "lib/libhipblaslt.so",
    "lib/libhsa-runtime64.so",
    "lib/liborigami.so",
    "lib/librocblas.so",
    "lib/librocm_kpack.so",
    "lib/librocprofiler-register.so",
    "lib/librocroller.so",
    "lib/librocsolver.so",
    "lib/libroctx64.so",
    "lib/hipblaslt/library/",
    "lib/rocblas/library/",
    "lib/llvm/lib/libLLVM.so",
    "lib/llvm/lib/libclang-cpp.so",
    "lib/rocm_sysdeps/lib/librocm_sysdeps_",
    "share/doc/",
];

/// The native `rocm-stable` channel's runtime (backend_versions.json `therock.version`),
/// downloaded from AMD's tarball repository. AMD publishes no digest; this one was
/// computed from the downloaded tarball.
pub const THEROCK_GFX1150: Runtime = Runtime {
    name: "therock",
    version: "10.1.0",
    gpu_target: "gfx1150",
    path: "rocm/core/tarball",
    asset: "therock-dist-linux-gfx1150-10.1.0.tar.gz",
    archive_bytes: 1972096149,
    sha256: "515df1170721668427a602807043719e59937109863e0baa7566875e9fccf070",
    format: ArchiveFormat::TarGz,
    directory: "rocm",
    members: THEROCK_MEMBERS,
};

/// Declared package licenses: the engine's own plus the vendor runtime its backend bundles.
pub fn licenses(build: &Build) -> Vec<String> {
    let vendor: &[&str] = match build.backend {
        "hybrid-rocm" => &["LicenseRef-AMD-ROCm", "LicenseRef-AMD-XRT"],
        "cuda" => &["LicenseRef-NVIDIA-CUDA-Redistributable"],
        "rocm" | "hrx" => &["LicenseRef-AMD-ROCm"],
        _ => &[],
    };
    vendor
        .iter()
        .chain(build.engine.info().licenses)
        .map(|license| (*license).to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Every published build: text engines first, then media engines.
pub fn builds() -> impl Iterator<Item = &'static Build> {
    llm::BUILDS.iter().chain(media::BUILDS)
}

/// Engines with a native support row but no published release asset; they install only
/// through a runtime-package import.
pub const UNPUBLISHED: &[(Engine, &str)] = &[];

/// Native `system` runs a llama.cpp the operator placed on the host; the native image put its
/// Vulkan build there (`/opt/juiced/llama`), so the managed Vulkan build stands in for it.
pub fn canonical_backend(engine: Engine, backend: &str) -> &str {
    match (engine, backend) {
        (Engine::LlamaCpp, "system") => "vulkan",
        _ => backend,
    }
}

/// Concrete ISAs that releases publish under a family asset; others ship verbatim.
pub fn rocm_asset_family(target: &str) -> &str {
    match target {
        "gfx1200" | "gfx1201" => "gfx120X",
        "gfx1100" | "gfx1101" | "gfx1102" | "gfx1103" => "gfx110X",
        "gfx1030" | "gfx1031" | "gfx1032" | "gfx1033" | "gfx1034" | "gfx1035" | "gfx1036" => {
            "gfx103X"
        }
        other => other,
    }
}

fn sm_value(target: &str) -> Option<u32> {
    target.strip_prefix("sm_")?.parse().ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unresolved {
    HostUnsupported,
    BackendUnknown,
    NoMatchingGpu,
    NotPublished,
    NoMatchingNpu,
}

/// Multi-GPU hosts get the highest CUDA compute capability with a published build and a
/// discrete AMD GPU over an integrated one, matching the native installer.
pub fn resolve(engine: Engine, backend: &str, host: &HostReport) -> Result<Build, Unresolved> {
    let candidates: Vec<&Build> = builds()
        .filter(|build| build.engine == engine && build.backend == backend)
        .collect();
    if candidates.is_empty() {
        return Err(if UNPUBLISHED.contains(&(engine, backend)) {
            Unresolved::NotPublished
        } else {
            Unresolved::BackendUnknown
        });
    }
    let candidates: Vec<&Build> = candidates
        .into_iter()
        .filter(|build| build.os == host.host.os && build.architecture == host.host.architecture)
        .collect();
    if candidates.is_empty() {
        return Err(Unresolved::HostUnsupported);
    }
    if let Some(build) = candidates.iter().find(|build| build.gpu_target.is_none()) {
        return Ok(**build);
    }
    let gpus = &host.devices.gpus;
    match backend {
        "cuda" => gpus
            .iter()
            .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
            .filter_map(|gpu| gpu.family.as_deref())
            .filter_map(|family| {
                candidates
                    .iter()
                    .find(|build| build.gpu_target == Some(family))
                    .map(|build| (sm_value(family).unwrap_or(0), **build))
            })
            .max_by_key(|(value, _)| *value)
            .map(|(_, build)| build)
            .ok_or(Unresolved::NoMatchingGpu),
        "rocm" => {
            let mut amd: Vec<_> = gpus
                .iter()
                .filter(|gpu| gpu.vendor == GpuVendor::Amd)
                .collect();
            amd.sort_by_key(|gpu| gpu.integrated == Some(true));
            amd.iter()
                .filter_map(|gpu| gpu.family.as_deref())
                .find_map(|family| {
                    let asset = rocm_asset_family(family);
                    candidates
                        .iter()
                        .find(|build| build.gpu_target == Some(asset))
                        .map(|build| **build)
                })
                .ok_or(Unresolved::NoMatchingGpu)
        }
        "hybrid-rocm" => {
            if !host
                .devices
                .npus
                .iter()
                .any(|npu| npu.vendor == NpuVendor::Amd && npu.family == Some("XDNA2"))
            {
                return Err(Unresolved::NoMatchingNpu);
            }
            gpus.iter()
                .filter(|gpu| gpu.vendor == GpuVendor::Amd)
                .filter_map(|gpu| gpu.family.as_deref())
                .find_map(|family| {
                    candidates
                        .iter()
                        .find(|build| build.gpu_target == Some(family))
                        .map(|build| **build)
                })
                .ok_or(Unresolved::NoMatchingGpu)
        }
        _ => Err(Unresolved::NoMatchingGpu),
    }
}
