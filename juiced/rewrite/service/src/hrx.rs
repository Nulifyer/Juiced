//! llama.cpp HRX build contract (native recipe `llamacpp-hrx`).
//!
//! The HRX build is llama-server with AMD's HRX ggml backend. Native Juiced started it with
//! `--device HRX0` and Vulkan disabled, because the bundle also ships the Vulkan backend
//! and ggml would otherwise schedule onto both.

use crate::{config::Provider, runtime_bundle::VerifiedBundle};

pub const BACKEND: &str = "hrx";
pub const DEVICE: [&str; 2] = ["--device", "HRX0"];

/// A llama.cpp bundle is the HRX build when it ships the HRX ggml backend.
pub fn bundle(adapter: Provider, bundle: Option<&VerifiedBundle>) -> bool {
    adapter == Provider::LlamaCpp
        && bundle.is_some_and(|bundle| {
            bundle.files.keys().any(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("libggml-hrx.so"))
            })
        })
}

pub fn environment() -> (String, String) {
    ("GGML_DISABLE_VULKAN".into(), "1".into())
}
