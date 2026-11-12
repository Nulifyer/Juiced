use crate::hardware::{GpuVendor, HostReport, NpuVendor};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    TarGz,
    TarXz,
    Zip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    #[serde(rename = "llama.cpp")]
    LlamaCpp,
    #[serde(rename = "whisper.cpp")]
    WhisperCpp,
    #[serde(rename = "sd.cpp")]
    StableDiffusionCpp,
    #[serde(rename = "kokoros")]
    Kokoros,
    #[serde(rename = "parakeet.cpp")]
    Parakeet,
    #[serde(rename = "moonshine")]
    Moonshine,
    #[serde(rename = "futo-transcribe.cpp")]
    Asr4all,
    #[serde(rename = "fastflowlm")]
    FastFlowLm,
}

impl Engine {
    pub const ALL: [Self; 8] = [
        Self::LlamaCpp,
        Self::WhisperCpp,
        Self::StableDiffusionCpp,
        Self::Kokoros,
        Self::Parakeet,
        Self::Moonshine,
        Self::Asr4all,
        Self::FastFlowLm,
    ];
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "llama.cpp" | "llamacpp" | "llama-cpp" => Some(Self::LlamaCpp),
            "whisper.cpp" | "whispercpp" | "whisper-cpp" => Some(Self::WhisperCpp),
            "sd.cpp" | "sd-cpp" | "sdcpp" | "stable-diffusion.cpp" | "stable-diffusion-cpp" => {
                Some(Self::StableDiffusionCpp)
            }
            "kokoros" | "kokoro" => Some(Self::Kokoros),
            "parakeet.cpp" | "parakeet" | "parakeet-redux" => Some(Self::Parakeet),
            "moonshine" | "moonshine-voice" => Some(Self::Moonshine),
            "futo-transcribe.cpp" | "futo-transcribe-cpp" | "asr4all" => Some(Self::Asr4all),
            "fastflowlm" | "flm" => Some(Self::FastFlowLm),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama.cpp",
            Self::WhisperCpp => "whisper.cpp",
            Self::StableDiffusionCpp => "sd.cpp",
            Self::Kokoros => "kokoros",
            Self::Parakeet => "parakeet.cpp",
            Self::Moonshine => "moonshine",
            Self::Asr4all => "futo-transcribe.cpp",
            Self::FastFlowLm => "fastflowlm",
        }
    }
    pub fn recipe(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llamacpp",
            Self::WhisperCpp => "whispercpp",
            Self::StableDiffusionCpp => "sd-cpp",
            Self::Kokoros => "kokoro",
            Self::Parakeet => "parakeet",
            Self::Moonshine => "moonshine",
            Self::Asr4all => "asr4all",
            Self::FastFlowLm => "flm",
        }
    }
    pub fn provider(self) -> crate::config::Provider {
        match self {
            Self::LlamaCpp => crate::config::Provider::LlamaCpp,
            Self::WhisperCpp => crate::config::Provider::WhisperCpp,
            Self::StableDiffusionCpp => crate::config::Provider::StableDiffusionCpp,
            Self::Kokoros => crate::config::Provider::Kokoros,
            Self::Parakeet => crate::config::Provider::Parakeet,
            Self::Moonshine => crate::config::Provider::Moonshine,
            Self::Asr4all => crate::config::Provider::Asr4all,
            Self::FastFlowLm => crate::config::Provider::FastFlowLm,
        }
    }
    pub fn entrypoint(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama-server",
            Self::WhisperCpp => "whisper-server",
            Self::StableDiffusionCpp => "sd-server",
            Self::Kokoros => "koko",
            Self::Parakeet => "parakeet-server",
            Self::Moonshine => "moonshine-server",
            Self::Asr4all => "futo-server",
            Self::FastFlowLm => "flm-real",
        }
    }
    /// The model format alone selects these engines and their only native support row is
    /// experimental, so no core backend exists that automatic selection could prefer.
    pub fn accepts_experimental(self) -> bool {
        matches!(self, Self::Parakeet | Self::Asr4all)
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

const LLAMA_GGML: &str = "ggml-org/llama.cpp";
const LLAMA_GGML_TAG: &str = "b12032";
const LLAMA_LEMONADE: &str = "lemonade-sdk/llama.cpp";
const LLAMA_CUDA_TAG: &str = "b11395";
const LLAMA_ROCM_STABLE_TAG: &str = "b11580";
const LLAMA_ROCM: &str = "lemonade-sdk/llamacpp-rocm";
const LLAMA_ROCM_TAG: &str = "b1338";
const WHISPER: &str = "lemonade-sdk/whisper.cpp-rocm";
const WHISPER_TAG: &str = "v1.8.4";
const SD: &str = "lemonade-sdk/stable-diffusion.cpp";
const SD_TAG: &str = "master-957-90e86d1";
const KOKOROS: &str = "lemonade-sdk/Kokoros";
const KOKOROS_TAG: &str = "b21";
const PARAKEET: &str = "mudler/parakeet.cpp";
const PARAKEET_TAG: &str = "v0.6.1";
const MOONSHINE: &str = "lemonade-sdk/moonshine-server-rocm";
const MOONSHINE_TAG: &str = "moonshine0.0.62";
const FLM: &str = "ROCm/FastFlowLM";
const FLM_TAG: &str = "v1.0.12";
const HYBRID: &str = "Nulifyer/llama-xdna-hybrid";
const HYBRID_TAG: &str = "rocm-v0.3.2";

const fn linux(
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
    }
}

const fn with_runtime(build: Build, runtime: &'static Runtime) -> Build {
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

macro_rules! cuda {
    ($sm:literal, $bytes:literal, $sha:literal) => {
        linux(
            Engine::LlamaCpp,
            "cuda",
            Some($sm),
            (LLAMA_LEMONADE, LLAMA_CUDA_TAG),
            (
                concat!("llama-b11395-ubuntu-cuda-", $sm, "-x64.tar.xz"),
                $bytes,
                $sha,
            ),
            ArchiveFormat::TarXz,
        )
    };
}
macro_rules! rocm {
    ($target:literal, $bytes:literal, $sha:literal) => {
        linux(
            Engine::LlamaCpp,
            "rocm",
            Some($target),
            (LLAMA_ROCM, LLAMA_ROCM_TAG),
            (
                concat!("llama-b1338-ubuntu-rocm-", $target, "-x64.zip"),
                $bytes,
                $sha,
            ),
            ArchiveFormat::Zip,
        )
    };
}
macro_rules! whisper_rocm {
    ($target:literal, $bytes:literal, $sha:literal) => {
        linux(
            Engine::WhisperCpp,
            "rocm",
            Some($target),
            (WHISPER, WHISPER_TAG),
            (
                concat!("whisper-v1.8.4-linux-rocm-", $target, ".tar.gz"),
                $bytes,
                $sha,
            ),
            ArchiveFormat::TarGz,
        )
    };
}

macro_rules! sd_cuda {
    ($sm:literal, $bytes:literal, $sha:literal) => {
        linux(
            Engine::StableDiffusionCpp,
            "cuda",
            Some($sm),
            (SD, SD_TAG),
            (
                concat!("sd-master-90e86d1-ubuntu-cuda-", $sm, "-x64.tar.xz"),
                $bytes,
                $sha,
            ),
            ArchiveFormat::TarXz,
        )
    };
}

/// Digests are the `sha256:` values GitHub publishes for each release asset.
pub const BUILDS: &[Build] = &[
    linux(
        Engine::LlamaCpp,
        "cpu",
        None,
        (LLAMA_GGML, LLAMA_GGML_TAG),
        (
            "llama-b12032-bin-ubuntu-x64.tar.gz",
            22886629,
            "9f10432ca3f66ef003a7a540aaffb9da0acadaefd05404521b9ab7a2436af304",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::LlamaCpp,
        "vulkan",
        None,
        (LLAMA_GGML, LLAMA_GGML_TAG),
        (
            "llama-b12032-bin-ubuntu-vulkan-x64.tar.gz",
            35669614,
            "f75e7107f9450034c5467492c41e9ed91816e2d815fc4b802c6bd58de7d5b126",
        ),
        ArchiveFormat::TarGz,
    ),
    cuda!(
        "sm_75",
        606309716,
        "a30bd634b853b1f571fea4d623ab4eb3451b8e79caf525ad17eea5ac0c59e9a6"
    ),
    cuda!(
        "sm_80",
        605500556,
        "5f5788d7e077fd75047a102cfa3cc55a18cad2f75a86c89db05b5a94242fe624"
    ),
    cuda!(
        "sm_86",
        604476036,
        "f71a117ffdc2802881dac2213d35b722e4bc4fde89362b6fa1f938986d2ea8fc"
    ),
    cuda!(
        "sm_89",
        604397304,
        "03401420369ca70b87bb5f6632dd5373d5afe7467022eaca7376b5b6b9f074ee"
    ),
    cuda!(
        "sm_90",
        605820976,
        "cc02fcb2370980b2f67feafe1592858d7dc27c4e336c739565eb6a9523b73cbd"
    ),
    cuda!(
        "sm_100",
        607293364,
        "24315868d7651b98e433cab94b8ada116f5eaa5e56f4d917f807da52b8e8913e"
    ),
    cuda!(
        "sm_120",
        607784448,
        "cffd05da1d68793e2131224af2ed437e2400d5ee0cc20c7cd10804e977092457"
    ),
    cuda!(
        "sm_121",
        607930700,
        "13a37a2ab942536523f70179897f98e2d8d875314f5a0d26746751b28605f8b2"
    ),
    rocm!(
        "gfx103X",
        489088482,
        "a808ad265cba0cf50015eb8eec24a46fa5c737c96779d0327fca8d912c5fbde5"
    ),
    rocm!(
        "gfx110X",
        546622892,
        "a43dcd359cfb069c3bb7c7e2012bf35f87ee14a5448635bbbe581128bff038da"
    ),
    with_runtime(
        linux(
            Engine::LlamaCpp,
            "rocm",
            Some("gfx1150"),
            (LLAMA_LEMONADE, LLAMA_ROCM_STABLE_TAG),
            (
                "llama-b11580-bin-ubuntu-rocm-10.1-x64.tar.gz",
                287482076,
                "7655f6799c85f62801051b375e3ac4da98e34712f98279f8d300b1586a0325bd",
            ),
            ArchiveFormat::TarGz,
        ),
        &THEROCK_GFX1150,
    ),
    rocm!(
        "gfx1151",
        404255859,
        "b59d3824355dd48790976a6c35a486aecdd0d9804853ac4326a731fc26db57b0"
    ),
    rocm!(
        "gfx120X",
        819693131,
        "6727bbd3b09a668ad35a3c71b4d59e768697354d9c88bb74543c8dc50efab35e"
    ),
    rocm!(
        "gfx908",
        403567497,
        "23ac421cc21f3b629b62d6ebd07463eedf0e4fa386dc3e6ac71a074d3dc5e1a8"
    ),
    rocm!(
        "gfx90a",
        517492872,
        "95616d43405f197bebdaffd9d74b40a295ec8da33458fa311e807ddd53dd757f"
    ),
    linux(
        Engine::WhisperCpp,
        "cpu",
        None,
        (WHISPER, WHISPER_TAG),
        (
            "whisper-v1.8.4-linux-cpu-x86_64.tar.gz",
            4365860,
            "88df3f571d58871cd3f43e4a028df5dfe1463b24f8fd7df5dc4b7a2fb068ea0d",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::WhisperCpp,
        "vulkan",
        None,
        (WHISPER, WHISPER_TAG),
        (
            "whisper-v1.8.4-linux-vulkan-x86_64.tar.gz",
            43770408,
            "55d0bdf9be7092ed8148b27d26353f5755446381eb9e1e5b6b00d5c761d813b1",
        ),
        ArchiveFormat::TarGz,
    ),
    whisper_rocm!(
        "gfx110X",
        242211592,
        "16a57d2a735ef795e91764b7531f68ce3b9a2cd83d83fbe0bf33a72d842627ef"
    ),
    whisper_rocm!(
        "gfx1150",
        163015148,
        "6fc0c151031bcbcf41469a531d585da8c63a6e782c33276404131f73da67a13a"
    ),
    whisper_rocm!(
        "gfx1151",
        165015634,
        "efe7690aa11557309df727b9a4663c1e5e9910f26f6755b9226fcdaa92657e74"
    ),
    whisper_rocm!(
        "gfx120X",
        551429010,
        "9d136701ceff188a6727ec7e80b005d77a464e1bc7dc4c2ea2815bebe3788290"
    ),
    linux(
        Engine::StableDiffusionCpp,
        "cpu",
        None,
        (SD, SD_TAG),
        (
            "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64.zip",
            26130363,
            "763058cdf90250c0cd9d351b3c3c71d36962150d2e02d85948ac9bb40038527e",
        ),
        ArchiveFormat::Zip,
    ),
    with_runtime(
        linux(
            Engine::StableDiffusionCpp,
            "rocm",
            Some("gfx1150"),
            (SD, SD_TAG),
            (
                "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64-rocm-10.1.0.zip",
                236552254,
                "8c5605c92fc2605f1c96653d76e1ccb6c1a6aa998ba63d581de41143071ac720",
            ),
            ArchiveFormat::Zip,
        ),
        &THEROCK_GFX1150,
    ),
    linux(
        Engine::StableDiffusionCpp,
        "vulkan",
        None,
        (SD, SD_TAG),
        (
            "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64-vulkan.zip",
            36995490,
            "9af42e767d12b149648b9bdc44390921a91ac138c0873f8823296f3856cea965",
        ),
        ArchiveFormat::Zip,
    ),
    sd_cuda!(
        "sm_75",
        584578272,
        "2f9f669e599152beac04f78b47fb06a9334bf1e6ddfef60766207d53d69a0c06"
    ),
    sd_cuda!(
        "sm_80",
        583804924,
        "13fd656d22ee87d895712b55368d480293e7fa5036ca0eadd88c15f15b480d7d"
    ),
    sd_cuda!(
        "sm_86",
        582930992,
        "4d9a4c4ba92c8cd2062170e51c589ce4cfb2f5826e5ea95f9bc60b36d37ff358"
    ),
    sd_cuda!(
        "sm_89",
        582907468,
        "bd395f10cfb1dd8b401bac4c00dcd8fdffa09b68c1f0fe2b76a6e7d799e087c4"
    ),
    sd_cuda!(
        "sm_90",
        584185972,
        "8ac73c3d35e545f6811a9ff5e6acc2a74043cbb30c24965e1c04f19a54137ad1"
    ),
    sd_cuda!(
        "sm_100",
        585864468,
        "50dba32391c64701fde3c365bc972966877cab847ca2b366fee8b9312223ef90"
    ),
    sd_cuda!(
        "sm_120",
        585048376,
        "20b6e9136bf93b484c63340f3fca8f2256d5509f206d67ff67818df217b8612b"
    ),
    sd_cuda!(
        "sm_121",
        585213856,
        "216202b21427afa4b747f9d25ff79ebfa195f04a84aa1599ba8653985ecd2bbd"
    ),
    linux(
        Engine::Kokoros,
        "cpu",
        None,
        (KOKOROS, KOKOROS_TAG),
        (
            "kokoros-linux-x86_64.tar.gz",
            21310353,
            "7dc102345a7b6fd2ac58a4296101996521970857e634353f78bf1861fdc1c996",
        ),
        ArchiveFormat::TarGz,
    ),
    // v0.6.1 is five commits past the native image's da8bb4c4 (= v0.6.0, which has no
    // assets); none of them touch the server.
    linux(
        Engine::Parakeet,
        "cpu",
        None,
        (PARAKEET, PARAKEET_TAG),
        (
            "parakeet-v0.6.1-bin-linux-cpu-x64.tar.gz",
            2727511,
            "cce60d122ab72e1068cd0d164e54a21655a0b83f1b9c21befc20124f5a972c10",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::Moonshine,
        "cpu",
        None,
        (MOONSHINE, MOONSHINE_TAG),
        (
            "moonshine-server-moonshine0.0.62-linux-x64.tar.gz",
            132320778,
            "c19d6f333002af3209ee029be0e4f5967926edf29395de7401c83b5f9fc21191",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::FastFlowLm,
        "npu",
        None,
        (FLM, FLM_TAG),
        (
            "fastflowlm_1.0.12_linux.tar.gz",
            45752536,
            "626fbee5e5206a215a8c2c4d3d961055cbf7289d257d36599e9fef38f2f555a8",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::LlamaCpp,
        "hybrid-rocm",
        Some("gfx1150"),
        (HYBRID, HYBRID_TAG),
        (
            "llama-xdna-hybrid-linux-rocm-gfx1150-x86_64.tar.gz",
            1069727615,
            "f9d2825c59a2c7a755c36a6482a20525ebf16521441e1b3b4860c709c1016a1b",
        ),
        ArchiveFormat::TarGz,
    ),
];

/// Engines with a native support row but no published release asset; they install only
/// through a runtime-package import.
pub const UNPUBLISHED: &[(Engine, &str)] = &[(Engine::Asr4all, "cpu")];

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
    let candidates: Vec<&Build> = BUILDS
        .iter()
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
