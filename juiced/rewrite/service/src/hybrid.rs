//! llama-xdna-hybrid launch contract.
//!
//! The bundle's `hybrid-server` script only prepares an environment and fixed flags before
//! exec'ing its llama-server. Juiced starts that llama-server directly with the same
//! environment and flags so process identity still names a verified bundle executable. The
//! ggml XDNA backend reads its settings only from the environment, so the profile carries
//! them as manager-owned pseudo-flags that never reach llama-server.

use crate::{config::Provider, runtime_bundle::VerifiedBundle};
use std::path::Path;

pub const PLUGIN: &str = "libggml-xdna.so";
pub const MEMORY_FLAG: &str = "--hybrid-memory-mib";
pub const MIN_PROMPT_FLAG: &str = "--hybrid-min-prompt";
pub const DEVICE_FLAG: &str = "--hybrid-gpu-device";
pub const DEFAULT_MEMORY_MIB: u32 = 2048;
pub const DEFAULT_MIN_PROMPT: u32 = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub environment: Vec<(String, String)>,
    /// Flags `hybrid-server` places before the operator's arguments when the NPU is required.
    pub prefix: Vec<String>,
}

pub fn flag(name: &str) -> bool {
    matches!(name, MEMORY_FLAG | MIN_PROMPT_FLAG | DEVICE_FLAG)
}

fn device(value: &str) -> bool {
    value.strip_prefix("ROCm").is_some_and(|index| {
        !index.is_empty() && index.len() <= 3 && index.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// A llama.cpp bundle is the hybrid engine when its entrypoint ships the XDNA plugin.
pub fn root(adapter: Provider, bundle: Option<&VerifiedBundle>) -> Option<&Path> {
    let bundle = bundle.filter(|_| adapter == Provider::LlamaCpp)?;
    let directory = bundle.executable.parent()?;
    bundle
        .files
        .contains_key(&directory.join(PLUGIN))
        .then_some(directory)
}

/// Removes the hybrid pseudo-flags from llama.cpp arguments and returns what
/// `hybrid-server` and native Juiced set for the same options.
pub fn apply(root: Option<&Path>, arguments: &mut Vec<String>) -> Result<Launch, Error> {
    let mut memory = DEFAULT_MEMORY_MIB;
    let mut min_prompt = DEFAULT_MIN_PROMPT;
    let mut gpu = "ROCm0".to_owned();
    let mut kept = Vec::with_capacity(arguments.len());
    let mut position = 0;
    while position < arguments.len() {
        let (name, inline) = arguments[position]
            .split_once('=')
            .map_or((arguments[position].as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        if !flag(name) {
            kept.push(arguments[position].clone());
            position += 1;
            continue;
        }
        if root.is_none() {
            return Err(Error {
                code: "hybrid_options_unsupported",
                message: "Hybrid NPU options require the llama.cpp hybrid-rocm engine package.",
            });
        }
        let value = inline
            .or_else(|| arguments.get(position + 1).map(String::as_str))
            .ok_or(invalid())?;
        match name {
            MEMORY_FLAG => {
                memory = value
                    .parse()
                    .ok()
                    .filter(|value| (128..=16384).contains(value))
                    .ok_or(invalid())?
            }
            MIN_PROMPT_FLAG => {
                min_prompt = value
                    .parse()
                    .ok()
                    .filter(|value| (1024..=32768).contains(value))
                    .ok_or(invalid())?
            }
            _ if device(value) => gpu = value.to_owned(),
            _ => return Err(invalid()),
        }
        position += if inline.is_some() { 1 } else { 2 };
    }
    *arguments = kept;
    let Some(root) = root else {
        return Ok(Launch::default());
    };
    let path = |relative: &str| root.join(relative).to_string_lossy().into_owned();
    let library_path = ["rocm/lib", "xrt/lib/x86_64-linux-gnu", "xrt/lib", ""]
        .map(path)
        .join(":");
    let environment = vec![
        ("LD_LIBRARY_PATH".into(), library_path),
        ("GGML_BACKEND_PATH".into(), path(PLUGIN)),
        ("GGML_XDNA_FAIL_CLOSED".into(), "1".into()),
        ("GGML_XDNA_EXPLICIT_PHASE".into(), "1".into()),
        ("XILINX_XRT".into(), path("xrt")),
        ("GGML_XDNA_GPU_DEVICE".into(), gpu.clone()),
        ("HYBRID_GPU_DEVICE".into(), gpu.clone()),
        ("HYBRID_REQUIRE_NPU".into(), "1".into()),
        (
            "GGML_XDNA_MAX_COPY_GB".into(),
            format!("{:.6}", f64::from(memory) / 1024.0),
        ),
        ("GGML_XDNA_MIN_BATCH".into(), min_prompt.to_string()),
    ];
    let prefix = [
        "--device",
        &format!("XDNA0,{gpu}"),
        "--tensor-split",
        "0,1",
        "--n-gpu-layers",
        "99",
        "--mmproj-device",
        &gpu,
        "--batch-size",
        "2048",
        "--ubatch-size",
        "2048",
    ]
    .map(str::to_owned)
    .into();
    Ok(Launch {
        environment,
        prefix,
    })
}

fn invalid() -> Error {
    Error {
        code: "invalid_launch_options",
        message: "Hybrid memory must be 128-16384 MiB, minimum prefill 1024-32768 tokens, and the GPU device ROCm<N>.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn value<'a>(launch: &'a Launch, name: &str) -> &'a str {
        &launch
            .environment
            .iter()
            .find(|(key, _)| key == name)
            .unwrap_or_else(|| panic!("{name}"))
            .1
    }

    #[test]
    fn hybrid_options_become_the_wrapper_and_native_environment() {
        let root = Path::new("/packages/x/payload");
        let mut arguments = strings(&["--model", "/m.gguf", "--port", "9"]);
        let launch = apply(Some(root), &mut arguments).unwrap();
        assert_eq!(arguments, strings(&["--model", "/m.gguf", "--port", "9"]));
        assert_eq!(value(&launch, "HYBRID_GPU_DEVICE"), "ROCm0");
        assert_eq!(value(&launch, "GGML_XDNA_GPU_DEVICE"), "ROCm0");
        assert_eq!(value(&launch, "HYBRID_REQUIRE_NPU"), "1");
        assert_eq!(value(&launch, "GGML_XDNA_MAX_COPY_GB"), "2.000000");
        assert_eq!(value(&launch, "GGML_XDNA_MIN_BATCH"), "1024");
        assert_eq!(value(&launch, "GGML_XDNA_FAIL_CLOSED"), "1");
        assert_eq!(value(&launch, "GGML_XDNA_EXPLICIT_PHASE"), "1");
        assert_eq!(
            value(&launch, "GGML_BACKEND_PATH"),
            "/packages/x/payload/libggml-xdna.so"
        );
        assert_eq!(value(&launch, "XILINX_XRT"), "/packages/x/payload/xrt");
        assert_eq!(
            value(&launch, "LD_LIBRARY_PATH"),
            "/packages/x/payload/rocm/lib:/packages/x/payload/xrt/lib/x86_64-linux-gnu:/packages/x/payload/xrt/lib:/packages/x/payload/"
        );
        assert_eq!(
            launch.prefix,
            strings(&[
                "--device",
                "XDNA0,ROCm0",
                "--tensor-split",
                "0,1",
                "--n-gpu-layers",
                "99",
                "--mmproj-device",
                "ROCm0",
                "--batch-size",
                "2048",
                "--ubatch-size",
                "2048"
            ])
        );
        let mut arguments = strings(&[
            "--model",
            "/m.gguf",
            "--hybrid-memory-mib",
            "3072",
            "--hybrid-min-prompt=4096",
            "--hybrid-gpu-device",
            "ROCm1",
        ]);
        let launch = apply(Some(root), &mut arguments).unwrap();
        assert_eq!(arguments, strings(&["--model", "/m.gguf"]));
        assert_eq!(value(&launch, "HYBRID_GPU_DEVICE"), "ROCm1");
        assert_eq!(launch.prefix[1], "XDNA0,ROCm1");
        assert_eq!(value(&launch, "GGML_XDNA_MAX_COPY_GB"), "3.000000");
        assert_eq!(value(&launch, "GGML_XDNA_MIN_BATCH"), "4096");
    }

    #[test]
    fn hybrid_options_are_bounded_and_require_the_hybrid_bundle() {
        let root = Path::new("/p");
        for invalid in [
            &["--hybrid-memory-mib", "127"][..],
            &["--hybrid-memory-mib", "16385"],
            &["--hybrid-min-prompt", "1023"],
            &["--hybrid-min-prompt", "32769"],
            &["--hybrid-gpu-device", "Vulkan0"],
            &["--hybrid-gpu-device", "ROCm"],
            &["--hybrid-memory-mib"],
        ] {
            let mut arguments = strings(invalid);
            assert_eq!(
                apply(Some(root), &mut arguments).unwrap_err().code,
                "invalid_launch_options",
                "{invalid:?}"
            );
        }
        let mut arguments = strings(&["--hybrid-memory-mib", "2048"]);
        assert_eq!(
            apply(None, &mut arguments).unwrap_err().code,
            "hybrid_options_unsupported"
        );
        let mut arguments = strings(&["--model", "x"]);
        assert_eq!(apply(None, &mut arguments).unwrap(), Launch::default());
    }
}
