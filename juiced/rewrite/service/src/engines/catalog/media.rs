use super::{ArchiveFormat, Build, Engine, EngineInfo, THEROCK_GFX1150, linux, with_runtime};
use crate::config::Provider;

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
const FUTO: &str = "Nulifyer/Juiced";
const FUTO_TAG: &str = "engine-futo-26d75933c16b";

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

pub(super) const ENGINES: &[EngineInfo] = &[
    EngineInfo {
        engine: Engine::WhisperCpp,
        name: "whisper.cpp",
        aliases: &["whispercpp", "whisper-cpp"],
        recipe: "whispercpp",
        provider: Provider::WhisperCpp,
        entrypoint: "whisper-server",
        accepts_experimental: false,
        licenses: &["MIT"],
    },
    EngineInfo {
        engine: Engine::StableDiffusionCpp,
        name: "sd.cpp",
        aliases: &[
            "sd-cpp",
            "sdcpp",
            "stable-diffusion.cpp",
            "stable-diffusion-cpp",
        ],
        recipe: "sd-cpp",
        provider: Provider::StableDiffusionCpp,
        entrypoint: "sd-server",
        accepts_experimental: false,
        licenses: &["MIT"],
    },
    EngineInfo {
        engine: Engine::Kokoros,
        name: "kokoros",
        aliases: &["kokoro"],
        recipe: "kokoro",
        provider: Provider::Kokoros,
        entrypoint: "koko",
        accepts_experimental: false,
        licenses: &["Apache-2.0", "GPL-3.0-or-later"],
    },
    EngineInfo {
        engine: Engine::Parakeet,
        name: "parakeet.cpp",
        aliases: &["parakeet", "parakeet-redux"],
        recipe: "parakeet",
        provider: Provider::Parakeet,
        entrypoint: "parakeet-server",
        accepts_experimental: true,
        licenses: &["MIT"],
    },
    EngineInfo {
        engine: Engine::Moonshine,
        name: "moonshine",
        aliases: &["moonshine-voice"],
        recipe: "moonshine",
        provider: Provider::Moonshine,
        entrypoint: "moonshine-server",
        accepts_experimental: false,
        licenses: &["BSD-3-Clause", "MIT", "PSF-2.0"],
    },
    EngineInfo {
        engine: Engine::Asr4all,
        name: "futo-transcribe.cpp",
        aliases: &["futo-transcribe-cpp", "asr4all"],
        recipe: "asr4all",
        provider: Provider::Asr4all,
        entrypoint: "futo-server",
        accepts_experimental: true,
        licenses: &["MIT"],
    },
];

/// Digests are the `sha256:` values GitHub publishes for each release asset.
pub(super) const BUILDS: &[Build] = &[
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
    // FUTO publishes no builds; Juiced's engine release workflow builds the pinned source.
    linux(
        Engine::Asr4all,
        "cpu",
        None,
        (FUTO, FUTO_TAG),
        (
            "engine-futo-26d75933c16b-linux-x86_64.tar.gz",
            2167861,
            "17202f6584a42ad428cd7217f94a9adfdecd30bb75046e1fe4a267bcd126b1f4",
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
];
