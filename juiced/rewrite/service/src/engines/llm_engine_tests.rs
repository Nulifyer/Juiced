use super::*;
use crate::hardware::tests::{cpu_only_host, report, rtx_host, strix_host};
use catalog::{EnvironmentSource, Unresolved, resolve};
use sha2::{Digest, Sha256};

fn inventory(host: &HostReport) -> Inventory {
    crate::hardware::project(&crate::catalog::Catalog::from_seed().unwrap(), host)
}

fn variant<'a>(
    inventory: &'a Inventory,
    recipe: &str,
    backend: &str,
) -> &'a crate::hardware::BackendVariant {
    inventory
        .backends
        .iter()
        .find(|variant| variant.recipe == recipe && variant.backend == backend)
        .unwrap()
}

/// Strix Halo: the gfx1151 iGPU DS4 and HRX publish for.
fn strix_halo() -> HostReport {
    let tree = strix_host(true);
    tree.file(
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "cpu_cores_count 0\nsimd_count 16\ngfx_target_version 110501\nvendor_id 4098\ndevice_id 5390\ndrm_render_minor 128\n",
    );
    report(&tree)
}

#[test]
fn engine_metadata_round_trips_through_names_aliases_and_serde() {
    for engine in Engine::all() {
        assert_eq!(Engine::parse(engine.name()), Some(engine));
        let implementation = engine.provider().implementation_id().unwrap();
        assert_eq!(
            Engine::parse(implementation),
            Some(engine),
            "{implementation}"
        );
        let encoded = serde_json::to_value(engine).unwrap();
        assert_eq!(encoded, engine.name());
        assert_eq!(serde_json::from_value::<Engine>(encoded).unwrap(), engine);
    }
    assert!(serde_json::from_value::<Engine>(serde_json::json!("flm")).is_err());
    assert_eq!(catalog::recipe_of(Engine::LlamaCpp, "hrx"), "llamacpp-hrx");
    assert_eq!(catalog::recipe_of(Engine::LlamaCpp, "cuda"), "llamacpp");
    assert_eq!(Engine::all().count(), 11);
}

#[test]
fn environment_pins_are_exact_and_the_lock_matches_its_digest() {
    let mut environments = 0;
    for build in catalog::builds() {
        let Some(environment) = build.environment else {
            continue;
        };
        environments += 1;
        assert!(build.sha256.is_some(), "{}", build.asset);
        match &environment.source {
            EnvironmentSource::Uv {
                lock, lock_sha256, ..
            } => {
                assert_eq!(
                    &format!("{:x}", Sha256::digest(lock.as_bytes())),
                    lock_sha256
                );
                assert!(lock.contains("vllm==0.33.0 \\"));
                assert!(lock.contains("ziglang==0.17.1 \\"));
                for requirement in lock
                    .lines()
                    .filter(|line| !line.starts_with([' ', '#']) && !line.is_empty())
                {
                    assert!(
                        requirement.contains("==") && requirement.ends_with(" \\"),
                        "{requirement}"
                    );
                }
                assert!(
                    !lock
                        .lines()
                        .any(|line| line.starts_with("-e ") || line.contains("git+"))
                );
            }
            EnvironmentSource::Parts { parts } => {
                assert_eq!(parts.len(), 2, "{}", build.asset);
                assert_eq!(parts[0].asset, build.asset);
                assert_eq!(parts[0].sha256, build.sha256.unwrap());
                assert_eq!(parts[0].bytes, build.archive_bytes);
                assert!(parts.iter().all(|part| part.asset.starts_with(build.tag)));
            }
        }
        assert!(environment.pin().len() > 10);
    }
    assert_eq!(environments, 5);
}

#[test]
fn rtx_host_offers_vllm_cuda_and_onnxruntime_and_refuses_amd_only_engines() {
    let host = report(&rtx_host());
    let inventory = inventory(&host);
    let vllm = resolve(Engine::Vllm, "cuda", &host).unwrap();
    assert_eq!(vllm.repository, "astral-sh/uv");
    assert!(vllm.environment.is_some());
    assert_eq!(
        variant(&inventory, "vllm", "cuda").state,
        Availability::Available
    );
    assert_eq!(
        catalog::licenses(&vllm),
        [
            "Apache-2.0",
            "BSD-3-Clause",
            "LicenseRef-NVIDIA-CUDA-Redistributable",
            "PSF-2.0"
        ]
    );
    assert_eq!(
        resolve(Engine::Vllm, "rocm", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    assert_eq!(
        resolve(Engine::Ds4, "rocm", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    assert_eq!(
        variant(&inventory, "llamacpp-hrx", "hrx").state,
        Availability::Unavailable
    );
    let rows = installable(&inventory, &host, &[]);
    let hrx = rows
        .iter()
        .find(|row| row.engine == Engine::LlamaCpp && row.backend == "hrx")
        .unwrap();
    assert_eq!(hrx.availability, Some(Availability::Unavailable));
    assert!(!hrx.reasons.is_empty());
    let installed = [Installed {
        package: format!("environment-{}", "b".repeat(64)),
        engine: Some(Engine::Vllm),
        backend: Some("cuda".into()),
        gpu_target: None,
        tag: Some(vllm.tag.into()),
        installed: true,
        declared_host_match: true,
    }];
    let choice = choose(Engine::Vllm, &inventory, &host, &installed, None).unwrap();
    assert_eq!(choice.backend, "cuda");
    assert!(choice.gpu);
    assert_eq!(choice.package, installed[0].package);
    let onnx = resolve(Engine::OnnxRuntime, "cpu", &host).unwrap();
    assert_eq!(onnx.asset, "ort-server-0.3.7-linux-x64.tar.gz");
    assert_eq!(
        variant(&inventory, "onnxruntime", "cpu").state,
        Availability::Available
    );
}

#[test]
fn strix_point_offers_vllm_rocm_and_hybrid_but_not_gfx1151_only_engines() {
    let host = report(&strix_host(true));
    let inventory = inventory(&host);
    let vllm = resolve(Engine::Vllm, "rocm", &host).unwrap();
    assert_eq!(vllm.tag, "vllm0.20.1-rocm7.12.0-gfx1150");
    assert_eq!(
        vllm.asset,
        "vllm0.20.1-rocm7.12.0-gfx1150-x64.part01-of-02.tar.gz"
    );
    assert_eq!(
        variant(&inventory, "vllm", "rocm").state,
        Availability::Available
    );
    assert_eq!(
        variant(&inventory, "vllm", "cuda").state,
        Availability::Unavailable
    );
    assert_eq!(
        resolve(Engine::Ds4, "rocm", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    assert_eq!(
        variant(&inventory, "llamacpp-hrx", "hrx").state,
        Availability::Unavailable
    );
    assert_eq!(
        resolve(Engine::LlamaCpp, "hybrid-rocm", &host).unwrap().tag,
        "rocm-v0.3.2"
    );
    assert!(matches!(
        choose(Engine::Vllm, &inventory, &host, &[], None),
        Err(ChoiceError::NotInstalled {
            installable: true,
            ..
        })
    ));
}

#[test]
fn strix_halo_resolves_ds4_hrx_and_its_vllm_build() {
    let host = strix_halo();
    let inventory = inventory(&host);
    let ds4 = resolve(Engine::Ds4, "rocm", &host).unwrap();
    assert_eq!(ds4.asset, "ds4-b0001-linux-rocm-gfx1151-x64.tar.gz");
    assert_eq!(
        variant(&inventory, "ds4", "rocm").state,
        Availability::Available
    );
    assert_eq!(
        resolve(Engine::Vllm, "rocm", &host).unwrap().tag,
        "vllm0.20.1-rocm7.12.0-gfx1151"
    );
    assert_eq!(
        variant(&inventory, "llamacpp-hrx", "hrx").state,
        Availability::Available
    );
    assert_eq!(
        resolve(Engine::LlamaCpp, "hrx", &host).unwrap().asset,
        "llama-hrx-b99-bin-manylinux-hrx-x64.tar.gz"
    );
    let hrx = Installed {
        package: format!("package-{}", "c".repeat(64)),
        engine: Some(Engine::LlamaCpp),
        backend: Some("hrx".into()),
        gpu_target: None,
        tag: Some("hrx-b99".into()),
        installed: true,
        declared_host_match: true,
    };
    let choice = choose(Engine::LlamaCpp, &inventory, &host, &[hrx], Some("hrx")).unwrap();
    assert_eq!(choice.backend, "hrx");
    let ds4_package = Installed {
        package: format!("package-{}", "d".repeat(64)),
        engine: Some(Engine::Ds4),
        backend: Some("rocm".into()),
        gpu_target: Some("gfx1151".into()),
        tag: Some("b0001".into()),
        installed: true,
        declared_host_match: true,
    };
    let choice = choose(Engine::Ds4, &inventory, &host, &[ds4_package], None).unwrap();
    assert_eq!(choice.backend, "rocm");
}

#[test]
fn cpu_only_hosts_get_onnxruntime_and_reasons_for_every_gpu_engine() {
    let host = report(&cpu_only_host());
    let inventory = inventory(&host);
    assert_eq!(
        variant(&inventory, "onnxruntime", "cpu").state,
        Availability::Available
    );
    for (recipe, backend) in [
        ("vllm", "cuda"),
        ("vllm", "rocm"),
        ("ds4", "rocm"),
        ("llamacpp-hrx", "hrx"),
    ] {
        let row = variant(&inventory, recipe, backend);
        assert_eq!(row.state, Availability::Unavailable, "{recipe} {backend}");
        assert!(!row.reasons.is_empty(), "{recipe} {backend}");
    }
    let ryzenai = variant(&inventory, "ryzenai-llm", "npu");
    assert_ne!(ryzenai.state, Availability::Available);
    assert!(!ryzenai.reasons.is_empty());
}

#[test]
fn gguf_formats_offer_ds4_and_checkpoints_register_on_vllm() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("config.json"),
        r#"{"architectures":["Qwen3ForCausalLM"],"max_position_embeddings":40960,"num_hidden_layers":28,"num_attention_heads":16,"num_key_value_heads":8,"head_dim":128}"#,
    )
    .unwrap();
    std::fs::write(directory.path().join("model.safetensors"), vec![0; 1024]).unwrap();
    let config = directory.path().join("config.json");
    let (format, bytes) = models::detect(&config).unwrap();
    assert_eq!(bytes, 1024);
    assert_eq!(format.engine(), Engine::Vllm);
    assert_eq!(format.engines(), [Engine::Vllm]);
    let estimate = models::estimate(&format, bytes, models::context_tokens(&format));
    assert_eq!(estimate.context_tokens, Some(8192));
    let registration = models::registration(&models::ProfileInputs {
        id: "Qwen3-0.6B",
        model_file: &config,
        artifact: None,
        artifact_set: None,
        package: "unused",
        backend: "cuda",
        gpu: true,
        cpu_threads: None,
        format: &format,
        estimate: &estimate,
        companion_arguments: &[],
        llama: None,
    });
    let profile = &registration["execution_profile"];
    assert_eq!(profile["adapter"], "vllm");
    let args: Vec<String> = serde_json::from_value(profile["args"].clone()).unwrap();
    assert!(crate::vllm::validate(&args).is_ok(), "{args:?}");
    assert_eq!(crate::vllm::max_model_len(&args), Some(8192));
    assert_eq!(registration["max_inflight"], 4);
    assert_eq!(
        registration["operations"],
        serde_json::json!(["chat/completions", "completions", "responses"])
    );

    std::fs::write(directory.path().join("model.onnx"), b"\x08\x07onnx").unwrap();
    std::fs::write(directory.path().join("tokenizer.json"), b"{}").unwrap();
    std::fs::write(
        directory.path().join("config.json"),
        r#"{"architectures":["DistilBertForSequenceClassification"],"id2label":{"0":"safe","1":"phishing"}}"#,
    )
    .unwrap();
    let (classifier, _) = models::detect(&directory.path().join("model.onnx")).unwrap();
    assert_eq!(classifier.engine(), Engine::OnnxRuntime);
    assert_eq!(
        serde_json::to_value(&classifier).unwrap(),
        serde_json::json!({"format": "onnx-classifier", "architecture": "DistilBertForSequenceClassification", "labels": 2})
    );
    let mut ds4 =
        serde_json::json!({"resident_bytes": 90u64 << 30, "execution_profile": {"args": []}});
    models::ds4_registration(&mut ds4, Some(32768), 81 << 30);
    let args: Vec<String> =
        serde_json::from_value(ds4["execution_profile"]["args"].clone()).unwrap();
    assert_eq!(
        args,
        [
            "-m",
            "{model}",
            "--host",
            "127.0.0.1",
            "--port",
            "{port}",
            "-c",
            "32768",
            "--ssd-streaming"
        ]
    );
    assert_eq!(ds4["execution_profile"]["adapter"], "ds4");
    assert_eq!(ds4["resident_bytes"], (81u64 << 30) / 5);
    assert!(crate::runtime_arguments::validate(crate::config::Provider::Ds4, &args).is_ok());
}
