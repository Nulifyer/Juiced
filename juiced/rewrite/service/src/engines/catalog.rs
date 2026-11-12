use crate::hardware::{GpuVendor, HostReport};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    TarGz,
    TarXz,
    Zip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
}

impl Engine {
    pub const ALL: [Self; 4] = [
        Self::LlamaCpp,
        Self::WhisperCpp,
        Self::StableDiffusionCpp,
        Self::Kokoros,
    ];
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "llama.cpp" | "llamacpp" | "llama-cpp" => Some(Self::LlamaCpp),
            "whisper.cpp" | "whispercpp" | "whisper-cpp" => Some(Self::WhisperCpp),
            "sd.cpp" | "sd-cpp" | "sdcpp" | "stable-diffusion.cpp" | "stable-diffusion-cpp" => {
                Some(Self::StableDiffusionCpp)
            }
            "kokoros" | "kokoro" => Some(Self::Kokoros),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama.cpp",
            Self::WhisperCpp => "whisper.cpp",
            Self::StableDiffusionCpp => "sd.cpp",
            Self::Kokoros => "kokoros",
        }
    }
    pub fn recipe(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llamacpp",
            Self::WhisperCpp => "whispercpp",
            Self::StableDiffusionCpp => "sd-cpp",
            Self::Kokoros => "kokoro",
        }
    }
    pub fn provider(self) -> crate::config::Provider {
        match self {
            Self::LlamaCpp => crate::config::Provider::LlamaCpp,
            Self::WhisperCpp => crate::config::Provider::WhisperCpp,
            Self::StableDiffusionCpp => crate::config::Provider::StableDiffusionCpp,
            Self::Kokoros => crate::config::Provider::Kokoros,
        }
    }
    pub fn entrypoint(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama-server",
            Self::WhisperCpp => "whisper-server",
            Self::StableDiffusionCpp => "sd-server",
            Self::Kokoros => "koko",
        }
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
const LLAMA_GGML_TAG: &str = "b11393";
const LLAMA_CUDA: &str = "lemonade-sdk/llama.cpp";
const LLAMA_CUDA_TAG: &str = "b11395";
const LLAMA_ROCM: &str = "lemonade-sdk/llamacpp-rocm";
const LLAMA_ROCM_TAG: &str = "b1338";
const WHISPER: &str = "lemonade-sdk/whisper.cpp-rocm";
const WHISPER_TAG: &str = "v1.8.4";
const SD: &str = "lemonade-sdk/stable-diffusion.cpp";
const SD_TAG: &str = "master-957-90e86d1";
const KOKOROS: &str = "lemonade-sdk/Kokoros";
const KOKOROS_TAG: &str = "b21";

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
    }
}

macro_rules! cuda {
    ($sm:literal, $bytes:literal, $sha:literal) => {
        linux(
            Engine::LlamaCpp,
            "cuda",
            Some($sm),
            (LLAMA_CUDA, LLAMA_CUDA_TAG),
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
            "llama-b11393-bin-ubuntu-x64.tar.gz",
            17635134,
            "c9752767461bfba667d011f2f92657789b896dae0ad69d8d2af8b8018c38a097",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::LlamaCpp,
        "vulkan",
        None,
        (LLAMA_GGML, LLAMA_GGML_TAG),
        (
            "llama-b11393-bin-ubuntu-vulkan-x64.tar.gz",
            31580852,
            "ef5566ca69b8905b4fbec789fff1129fd8eb5e1f19aa3ced6840fa36da2c1542",
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
    rocm!(
        "gfx1150",
        399329044,
        "2f62764db35b1662c7f652bbb6fbfd49cb12b058a269469f4a9577d6405adf52"
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
];

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
}

/// Multi-GPU hosts get the highest CUDA compute capability with a published build and a
/// discrete AMD GPU over an integrated one, matching the native installer.
pub fn resolve(engine: Engine, backend: &str, host: &HostReport) -> Result<Build, Unresolved> {
    let candidates: Vec<&Build> = BUILDS
        .iter()
        .filter(|build| build.engine == engine && build.backend == backend)
        .collect();
    if candidates.is_empty() {
        return Err(Unresolved::BackendUnknown);
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
        _ => Err(Unresolved::NoMatchingGpu),
    }
}
