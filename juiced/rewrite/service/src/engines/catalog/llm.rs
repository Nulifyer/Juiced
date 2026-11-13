use super::{
    ArchiveFormat, Build, Engine, EngineInfo, Environment, EnvironmentSource, Part, SupportRow,
    THEROCK_GFX1150, arm64, linux, with_environment, with_runtime,
};
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
const HRX: &str = "ROCm/ggml-staging-automation";
const HRX_TAG: &str = "hrx-b99";
const DS4: &str = "lemonade-sdk/ds4-rocm";
const DS4_TAG: &str = "b0001";
const ORT: &str = "lemonade-sdk/ort-server";
const ORT_TAG: &str = "0.3.7";
const UV: &str = "astral-sh/uv";
const UV_TAG: &str = "0.13.7";
const VLLM_ROCM: &str = "lemonade-sdk/vllm-rocm";

const GIB: u64 = 1024 * 1024 * 1024;

/// vLLM 0.33.0 with PyTorch 2.14 for CUDA 13.0 (sm_75 through sm_121), installed by uv
/// from hash-pinned wheels.
pub(super) const VLLM_CUDA: Environment = Environment {
    version: "vllm 0.33.0 (CUDA 13.0)",
    source: EnvironmentSource::Uv {
        python: "3.12.16",
        torch_backend: "cu130",
        lock: include_str!("locks/vllm-0.33.0-cu130.txt"),
        lock_sha256: "f482578dc648f27fbe1dad1013eb1c576dbb8fca6c8a28f8206aa5d08d296d2c",
    },
    installed_bytes: 12 * GIB,
};

/// The relocatable vLLM ROCm environments the native image installed
/// (backend_versions.json `vllm.rocm`; the native support row has no CDNA targets), each published
/// as one `.tar.gz` in two parts.
macro_rules! vllm_rocm {
    ($target:literal, $version:literal, $tag:literal, ($first:literal, $first_sha:literal), ($second:literal, $second_sha:literal)) => {
        with_environment(
            linux(
                Engine::Vllm,
                "rocm",
                Some($target),
                (VLLM_ROCM, $tag),
                (
                    concat!($tag, "-x64.part01-of-02.tar.gz"),
                    $first,
                    $first_sha,
                ),
                ArchiveFormat::TarGz,
            ),
            &Environment {
                version: $version,
                source: EnvironmentSource::Parts {
                    parts: &[
                        Part {
                            asset: concat!($tag, "-x64.part01-of-02.tar.gz"),
                            bytes: $first,
                            sha256: $first_sha,
                        },
                        Part {
                            asset: concat!($tag, "-x64.part02-of-02.tar.gz"),
                            bytes: $second,
                            sha256: $second_sha,
                        },
                    ],
                },
                installed_bytes: 10 * GIB,
            },
        )
    };
}

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
    EngineInfo {
        engine: Engine::Vllm,
        name: "vllm",
        aliases: &[],
        recipe: "vllm",
        provider: Provider::Vllm,
        entrypoint: "python3",
        accepts_experimental: true,
        licenses: &["Apache-2.0", "BSD-3-Clause", "PSF-2.0"],
    },
    EngineInfo {
        engine: Engine::Ds4,
        name: "ds4",
        aliases: &["dwarfstar4", "ds4-server"],
        recipe: "ds4",
        provider: Provider::Ds4,
        entrypoint: "ds4-server",
        accepts_experimental: true,
        licenses: &["MIT"],
    },
    EngineInfo {
        engine: Engine::OnnxRuntime,
        name: "onnxruntime",
        aliases: &["ort-server", "onnx-runtime"],
        recipe: "onnxruntime",
        provider: Provider::OnnxRuntime,
        entrypoint: "ort-server",
        accepts_experimental: true,
        licenses: &["Apache-2.0", "MIT"],
    },
];

/// vLLM on NVIDIA has no native support row; the CUDA 13 wheels cover these targets.
pub(super) const SUPPLEMENTAL_SUPPORT_ROWS: &[SupportRow] = &[SupportRow {
    recipe: "vllm",
    backend: "cuda",
    tier: "experimental",
    format: "python",
    os: &["linux"],
    device_declaration: "nvidia_gpu (sm_75, sm_80, sm_86, sm_89, sm_90, sm_100, sm_120, sm_121)",
}];

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
    linux(
        Engine::LlamaCpp,
        "hrx",
        None,
        (HRX, HRX_TAG),
        (
            "llama-hrx-b99-bin-manylinux-hrx-x64.tar.gz",
            37305496,
            "02c88d96d39c607aefbb982e8b4f630785ec43c4416583834912ceaa63fd7624",
        ),
        ArchiveFormat::TarGz,
    ),
    // Upstream ds4 publishes no binaries; lemonade-sdk/ds4-rocm builds a pinned commit
    // and bundles the ROCm runtime. Only gfx1151 is published.
    linux(
        Engine::Ds4,
        "rocm",
        Some("gfx1151"),
        (DS4, DS4_TAG),
        (
            "ds4-b0001-linux-rocm-gfx1151-x64.tar.gz",
            164211638,
            "e63b7c9428fd10de75b3f69164eccd564a8e7261c2f9087cd8bec2b4b19a8ad1",
        ),
        ArchiveFormat::TarGz,
    ),
    linux(
        Engine::OnnxRuntime,
        "cpu",
        None,
        (ORT, ORT_TAG),
        (
            "ort-server-0.3.7-linux-x64.tar.gz",
            12715107,
            "b2cbee81bbcc66874cb0486db6b442b3fb98a88d6c0986a97e107d249b388e50",
        ),
        ArchiveFormat::TarGz,
    ),
    arm64(linux(
        Engine::OnnxRuntime,
        "cpu",
        None,
        (ORT, ORT_TAG),
        (
            "ort-server-0.3.7-linux-arm64.tar.gz",
            11553481,
            "693ca799963162e317dddf880e6dec5168b6b922bf207e88c6ceca18a166ed7d",
        ),
        ArchiveFormat::TarGz,
    )),
    with_environment(
        linux(
            Engine::Vllm,
            "cuda",
            None,
            (UV, UV_TAG),
            (
                "uv-x86_64-unknown-linux-gnu.tar.gz",
                16671812,
                "c5221ff41b7892bfc223543883600125f6a47fe845093e0f5bdff4f359196d83",
            ),
            ArchiveFormat::TarGz,
        ),
        &VLLM_CUDA,
    ),
    vllm_rocm!(
        "gfx1150",
        "vllm 0.20.1 (ROCm 7.12)",
        "vllm0.20.1-rocm7.12.0-gfx1150",
        (
            1992294400,
            "34a618d10d5869ab04e1388ec8bb67dbde69b9ef3eef0810d4b14dc60ee82174"
        ),
        (
            686323863,
            "354b8ffdf35c4eaea9a7db8513275e7e95da1d2d8e17195352ed9153f5294325"
        )
    ),
    vllm_rocm!(
        "gfx1151",
        "vllm 0.20.1 (ROCm 7.12)",
        "vllm0.20.1-rocm7.12.0-gfx1151",
        (
            1992294400,
            "51c35040208aa7cd7ee59030d5a5a98c2360dd27df827947b18a5bb3a1c6d0c7"
        ),
        (
            687298802,
            "a11b9b870a920ef06727b5137e05a6823ca82195a57e184909dcdfe3291bdf4e"
        )
    ),
    vllm_rocm!(
        "gfx110X",
        "vllm 0.20.1 (ROCm 7.12)",
        "vllm0.20.1-rocm7.12.0-gfx110X",
        (
            1992294400,
            "cc39c42f6262cf850acd07670d1fb97f82fe11ca75608093c3e88ad7d5f6525c"
        ),
        (
            779548404,
            "d0a1802c60d21b02a29371f3c1e3b4a62b787a0b65d31b2cebcfd9f75965aba9"
        )
    ),
    vllm_rocm!(
        "gfx120X",
        "vllm 0.20.1 (ROCm 7.12)",
        "vllm0.20.1-rocm7.12.0-gfx120X",
        (
            1992294400,
            "402c8a5a84c668930ef82d0e5ecfec7a1ea13cdc9ff1a112f44793e3dbc43803"
        ),
        (
            753100649,
            "415b391085e373138a11e353c0bd15508f833b4dea156011a2fe3b327ec377de"
        )
    ),
];
