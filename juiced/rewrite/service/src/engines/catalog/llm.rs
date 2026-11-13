use super::{ArchiveFormat, Build, Engine, EngineInfo, THEROCK_GFX1150, linux, with_runtime};
use crate::config::Provider;

const LLAMA_GGML: &str = "ggml-org/llama.cpp";
const LLAMA_GGML_TAG: &str = "b12032";
const LLAMA_LEMONADE: &str = "lemonade-sdk/llama.cpp";
const LLAMA_CUDA_TAG: &str = "b11395";
const LLAMA_ROCM_STABLE_TAG: &str = "b11580";
const LLAMA_ROCM: &str = "lemonade-sdk/llamacpp-rocm";
const LLAMA_ROCM_TAG: &str = "b1338";
const FLM: &str = "ROCm/FastFlowLM";
const FLM_TAG: &str = "v1.0.12";
const HYBRID: &str = "Nulifyer/llama-xdna-hybrid";
const HYBRID_TAG: &str = "rocm-v0.3.2";

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
pub(super) const ENGINES: &[EngineInfo] = &[
    EngineInfo {
        engine: Engine::LlamaCpp,
        name: "llama.cpp",
        aliases: &["llamacpp", "llama-cpp"],
        recipe: "llamacpp",
        provider: Provider::LlamaCpp,
        entrypoint: "llama-server",
        accepts_experimental: false,
        licenses: &["MIT"],
    },
    EngineInfo {
        engine: Engine::FastFlowLm,
        name: "fastflowlm",
        aliases: &["flm"],
        recipe: "flm",
        provider: Provider::FastFlowLm,
        entrypoint: "flm-real",
        accepts_experimental: false,
        licenses: &["LicenseRef-FastFlowLM-Binary", "MIT"],
    },
];

/// Digests are the `sha256:` values GitHub publishes for each release asset.
pub(super) const BUILDS: &[Build] = &[
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
