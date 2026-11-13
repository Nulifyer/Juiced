use super::*;
use crate::{
    artifacts::ArtifactRole,
    config::{Operation, Provider},
    hardware::tests::{cpu_only_host, report, rtx_host, strix_host},
};
use catalog::{Unresolved, resolve};
use models::{CompanionError, FormatError, ModelFormat};
use std::{io::Write, path::PathBuf};

fn inventory(host: &HostReport) -> Inventory {
    crate::hardware::project(&crate::catalog::Catalog::from_seed().unwrap(), host)
}

fn gguf(architecture: &str) -> tempfile::NamedTempFile {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    let key = b"general.architecture";
    bytes.extend((key.len() as u64).to_le_bytes());
    bytes.extend(key);
    bytes.extend(8u32.to_le_bytes());
    bytes.extend((architecture.len() as u64).to_le_bytes());
    bytes.extend(architecture.as_bytes());
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&bytes).unwrap();
    file
}

fn gfx1151() -> HostReport {
    let tree = strix_host(true);
    tree.file(
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "cpu_cores_count 0\nsimd_count 40\ngfx_target_version 110501\nvendor_id 4098\ndevice_id 5390\ndrm_render_minor 128\n",
    );
    report(&tree)
}

#[test]
fn generation_engines_resolve_pinned_releases_per_backend() {
    let mut rtx = report(&rtx_host());
    rtx.devices.cpu.as_mut().unwrap().isa.push("avx512f");
    for (engine, prefix, tag) in [
        (Engine::AceStep, "acestep", "v0.1.1"),
        (Engine::ThinkSound, "thinksound", "v0.1.2"),
        (Engine::Trellis, "trellis", "v0.4.3"),
        (Engine::OpenMoss, "moss-tts", "v0.3.0"),
    ] {
        for backend in ["cuda", "vulkan"] {
            let build = resolve(engine, backend, &rtx).unwrap();
            assert_eq!(build.asset, format!("{prefix}-{backend}-linux-x64.tar.gz"));
            assert_eq!(build.tag, tag);
            assert!(build.sha256.is_some_and(|digest| digest.len() == 64));
            assert!(build.runtime.is_none() && build.gpu_target.is_none());
        }
        assert_eq!(
            engine.provider().implementation_id(),
            Engine::parse(engine.name())
                .unwrap()
                .provider()
                .implementation_id()
        );
        assert!(engine.accepts_experimental(), "{}", engine.name());
    }
    let strix = report(&strix_host(true));
    for engine in [Engine::AceStep, Engine::ThinkSound, Engine::OpenMoss] {
        assert_eq!(
            resolve(engine, "rocm", &strix).unwrap_err(),
            Unresolved::BackendUnknown
        );
    }
    let trellis = resolve(Engine::Trellis, "rocm", &strix).unwrap();
    assert_eq!(trellis.gpu_target, Some("gfx1150"));
    assert_eq!(trellis.runtime.unwrap().gpu_target, "gfx1150");
    assert_eq!(
        resolve(Engine::Trellis, "rocm", &gfx1151()).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    let noise = resolve(Engine::TheNoise, "rocm", &strix).unwrap();
    assert_eq!(noise.asset, "thenoise-0.10.1-rocm10.1.0-gfx1150-x64.tar.gz");
    assert!(noise.runtime.is_none(), "TheNoise bundles its ROCm runtime");
    assert_eq!(
        resolve(Engine::TheNoise, "rocm", &gfx1151()).unwrap().asset,
        "thenoise-0.10.1-rocm10.1.0-gfx1151-x64.tar.gz"
    );
    assert_eq!(
        resolve(Engine::TheNoise, "rocm", &rtx).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
}

#[test]
fn experimental_generation_engines_pick_the_first_available_backend() {
    let rtx = report(&rtx_host());
    let inventory = inventory(&rtx);
    let installed = |backend: &str| Installed {
        package: format!("package-{backend}"),
        engine: Some(Engine::AceStep),
        backend: Some(backend.into()),
        gpu_target: None,
        tag: Some("v0.1.1".into()),
        installed: true,
        declared_host_match: true,
    };
    let choice = choose(
        Engine::AceStep,
        &inventory,
        &rtx,
        &[installed("vulkan"), installed("cuda")],
        None,
    )
    .unwrap();
    assert_eq!(choice.backend, "cuda");
    assert!(choice.gpu);
    assert!(
        choice
            .reason
            .starts_with("cuda is the first available acestep.cpp backend"),
        "{}",
        choice.reason
    );
    let cpu = report(&cpu_only_host());
    assert_eq!(
        choose(
            Engine::Trellis,
            &crate::engines::media_engine_tests::inventory(&cpu),
            &cpu,
            &[],
            None
        )
        .unwrap_err(),
        ChoiceError::NoUsableBackend
    );
    let rows = installable(&inventory, &rtx, &[]);
    let noise = rows
        .iter()
        .find(|row| row.engine == Engine::TheNoise)
        .unwrap();
    assert_eq!(noise.unresolved, Some(Unresolved::NoMatchingGpu));
    assert_eq!(
        noise.availability,
        Some(crate::hardware::Availability::Unavailable)
    );
}

#[test]
fn generation_primaries_are_detected_and_their_networks_refused_alone() {
    for (architecture, format) in [
        ("acestep-dit", ModelFormat::AceStepGguf {}),
        ("thinksound-mmdit", ModelFormat::ThinkSoundGguf {}),
        ("trellis2-ss-flow", ModelFormat::TrellisGguf {}),
    ] {
        assert_eq!(models::detect(gguf(architecture).path()).unwrap().0, format);
        assert!(format.directory());
        assert!(!format.standalone());
        assert_eq!(models::context_tokens(&format), None);
    }
    for component in [
        "acestep-vae",
        "acestep-lm",
        "thinksound-t5-tok",
        "trellis2-shape-dec",
        "birefnet-swinl",
        "moss_soundeffect",
    ] {
        assert_eq!(
            models::detect(gguf(component).path()).unwrap_err(),
            FormatError::Invalid(models::COMPONENT_NOT_PRIMARY),
            "{component}"
        );
    }
    assert!(matches!(
        models::detect(gguf("qwen3").path()).unwrap().0,
        ModelFormat::Gguf { .. }
    ));
    assert_eq!(
        models::moss_family(gguf("moss_tts_delay").path())
            .unwrap()
            .as_deref(),
        Some("moss_tts_delay")
    );
    assert_eq!(models::moss_family(gguf("qwen3").path()).unwrap(), None);
    assert_eq!(
        models::moss_sidecar("moss-soundeffect-2.0.gguf"),
        "moss-soundeffect-2.0.extras.gguf"
    );
}

#[test]
fn native_repository_checkpoints_select_the_generation_primary() {
    let directory = tempfile::tempdir().unwrap();
    for (name, architecture) in [
        ("birefnet.gguf", "birefnet-swinl"),
        ("ss_flow.gguf", "trellis2-ss-flow"),
        ("tex_dec.gguf", "trellis2-tex-dec"),
    ] {
        std::fs::copy(gguf(architecture).path(), directory.path().join(name)).unwrap();
    }
    std::fs::write(directory.path().join("README.md"), "x").unwrap();
    let found = crate::native_import::Weights {
        path: directory.path().join("birefnet.gguf"),
        bytes: 1,
        files: Vec::new(),
        selection: "first-weight",
        snapshot: Some("abc".into()),
    };
    let weights = models::generation_directory(found);
    assert_eq!(weights.path, directory.path().join("ss_flow.gguf"));
    assert_eq!(weights.files.len(), 3);
    assert_eq!(weights.selection, "generation-directory");
    assert_eq!(weights.snapshot.as_deref(), Some("abc"));
}

fn member(name: &str, role: ArtifactRole) -> models::Companion {
    models::Companion {
        name: name.into(),
        role,
    }
}

#[test]
fn generation_registrations_pass_the_argument_grammar() {
    let package = format!("package-{}", "d".repeat(64));
    let model = PathBuf::from("weights");
    let ace = vec![
        member("vae-BF16.gguf", ArtifactRole::Vae),
        member("Qwen3-Embedding-0.6B-Q8_0.gguf", ArtifactRole::Encoder),
    ];
    let moss = vec![member(
        "moss-soundeffect-2.0.extras.gguf",
        ArtifactRole::ExternalTensor,
    )];
    let trellis: Vec<_> = [
        "dinov3.gguf",
        "ss_dec.gguf",
        "shape_flow_512.gguf",
        "shape_dec.gguf",
        "tex_flow_512.gguf",
        "tex_dec.gguf",
    ]
    .into_iter()
    .map(|name| member(name, ArtifactRole::Decoder))
    .collect();
    let noise = vec![
        member("vae.safetensors", ArtifactRole::Vae),
        member("qwen.safetensors", ArtifactRole::Encoder),
    ];
    for (format, gpu, companions, operation, expected) in [
        (
            ModelFormat::AceStepGguf {},
            true,
            &ace,
            Operation::AudioGeneration,
            vec![
                "--models",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--keep-loaded",
            ],
        ),
        (
            ModelFormat::ThinkSoundGguf {},
            true,
            &ace,
            Operation::AudioGeneration,
            vec![
                "--dir",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
            ],
        ),
        (
            ModelFormat::TrellisGguf {},
            true,
            &trellis,
            Operation::Model3dGeneration,
            vec![
                "--models",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--res",
                "512",
                "--require-gpu",
            ],
        ),
        (
            ModelFormat::TrellisGguf {},
            false,
            &trellis,
            Operation::Model3dGeneration,
            vec![
                "--models",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--res",
                "512",
            ],
        ),
        (
            ModelFormat::MossGguf {
                family: "moss_soundeffect".into(),
            },
            true,
            &moss,
            Operation::AudioGeneration,
            vec![
                "--model",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--no-webui",
            ],
        ),
        (
            ModelFormat::MossGguf {
                family: "moss_tts_delay".into(),
            },
            true,
            &moss,
            Operation::Speech,
            vec![
                "--model",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--no-webui",
            ],
        ),
        (
            ModelFormat::TheNoiseDiffusion { architecture: None },
            true,
            &noise,
            Operation::ImageGeneration,
            vec![
                "serve",
                "--dit",
                "{model}",
                "--vae",
                "{member:vae.safetensors}",
                "--text-encoder",
                "{member:qwen.safetensors}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
            ],
        ),
    ] {
        let arguments = models::companion_arguments(&format, companions).unwrap();
        let estimate = models::estimate(&format, 3_000_000_000, None);
        assert!(estimate.resident_bytes > 3_000_000_000 + 512 * 1024 * 1024);
        let registration = models::registration(&models::ProfileInputs {
            id: "generation",
            model_file: &model,
            artifact: None,
            artifact_set: None,
            package: &package,
            backend: if gpu { "cuda" } else { "cpu" },
            gpu,
            cpu_threads: Some(8),
            format: &format,
            estimate: &estimate,
            companion_arguments: &arguments,
            llama: None,
        });
        let crate::config::Registration(spec) = serde_json::from_value(registration).unwrap();
        assert_eq!(spec.operations, [operation]);
        assert_eq!(spec.execution_profile.adapter, format.engine().provider());
        assert_eq!(spec.execution_profile.args, expected);
        let (template, _) = spec.execution_profile.resolved_arguments().unwrap();
        assert_eq!(template.arguments(), expected.as_slice());
        template
            .verify_members(
                companions
                    .iter()
                    .map(|member| (member.name.as_str(), member.role)),
            )
            .unwrap();
    }
    assert_eq!(
        models::companion_arguments(&ModelFormat::AceStepGguf {}, &[]).unwrap_err(),
        CompanionError::Required("generation_companions_required")
    );
    assert_eq!(
        models::companion_arguments(&ModelFormat::TrellisGguf {}, &trellis[..3]).unwrap_err(),
        CompanionError::Required("generation_companions_required")
    );
    assert_eq!(
        models::companion_arguments(
            &ModelFormat::TheNoiseDiffusion { architecture: None },
            &noise[..1]
        )
        .unwrap_err(),
        CompanionError::Required("thenoise_companions_required")
    );
    let duplicate = vec![
        member("vae.gguf", ArtifactRole::Vae),
        member("vae.gguf", ArtifactRole::Encoder),
    ];
    assert_eq!(
        models::companion_arguments(&ModelFormat::AceStepGguf {}, &duplicate).unwrap_err(),
        CompanionError::Unsupported
    );
    assert!(
        models::companion_arguments(
            &ModelFormat::ThinkSoundGguf {},
            &[member("voices.bin", ArtifactRole::Voice)]
        )
        .is_err()
    );
}

#[test]
fn generation_worker_arguments_reject_unmanaged_files_and_ports() {
    let check = |provider: Provider, arguments: &[&str]| {
        crate::runtime_arguments::validate(
            provider,
            &arguments
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert!(
        check(
            Provider::AceStep,
            &[
                "--models",
                "{model}",
                "--port",
                "{port}",
                "--max-seq",
                "4096",
                "--no-fa"
            ]
        )
        .is_ok()
    );
    assert_eq!(
        check(
            Provider::AceStep,
            &[
                "--models",
                "{model}",
                "--port",
                "{port}",
                "--adapters",
                "/tmp/x"
            ]
        )
        .unwrap_err()
        .code,
        "unbound_runtime_side_input"
    );
    assert_eq!(
        check(
            Provider::AceStep,
            &["--models", "/elsewhere", "--port", "{port}"]
        )
        .unwrap_err()
        .code,
        "invalid_runtime_binding"
    );
    assert_eq!(
        check(
            Provider::Trellis,
            &["-m", "{model}", "--port", "{port}", "-o", "x.glb"]
        )
        .unwrap_err()
        .code,
        "unbound_runtime_side_input"
    );
    assert!(
        check(
            Provider::Trellis,
            &[
                "-m",
                "{model}",
                "--port",
                "{port}",
                "--tex-res",
                "512",
                "--box-uv"
            ]
        )
        .is_ok()
    );
    assert_eq!(
        check(
            Provider::OpenMoss,
            &[
                "--model",
                "{model}",
                "--port",
                "{port}",
                "--voice-dir",
                "/v"
            ]
        )
        .unwrap_err()
        .code,
        "unbound_runtime_side_input"
    );
    assert_eq!(
        check(
            Provider::ThinkSound,
            &["--dir", "{model}", "--port", "{port}", "--t5", "/x"]
        )
        .unwrap_err()
        .code,
        "unsupported_runtime_argument"
    );
    assert_eq!(
        check(
            Provider::TheNoise,
            &["--dit", "{model}", "--port", "{port}"]
        )
        .unwrap_err()
        .code,
        "missing_runtime_binding"
    );
    assert_eq!(
        check(
            Provider::TheNoise,
            &[
                "serve",
                "--dit",
                "{model}",
                "--port",
                "{port}",
                "--lora-dir",
                "/l"
            ]
        )
        .unwrap_err()
        .code,
        "unbound_runtime_side_input"
    );
    let template = check(
        Provider::OpenMoss,
        &["--model", "{model}", "--port", "{port}"],
    )
    .unwrap();
    assert_eq!(
        template.arguments().last().map(String::as_str),
        Some("127.0.0.1")
    );
    assert!(
        template
            .verify_members([("x.gguf", ArtifactRole::Voice)])
            .is_err()
    );
}

#[test]
fn thenoise_bundles_get_their_larger_file_bounds() {
    let limits = archive::limits_for(Engine::TheNoise);
    assert!(limits.files >= 40_000 && limits.entries >= 65_536);
    assert_eq!(
        archive::limits_for(Engine::AceStep).files,
        crate::runtime_bundle::MAX_FILES
    );
    assert_eq!(
        crate::runtime_bundle::file_limits("thenoise").0,
        crate::runtime_bundle::LARGE_MAX_FILES
    );
}

#[test]
fn openmoss_cuda_needs_an_avx512_cpu() {
    let zen2 = report(&rtx_host());
    assert_eq!(
        resolve(Engine::OpenMoss, "cuda", &zen2).unwrap_err(),
        Unresolved::CpuUnsupported
    );
    assert!(resolve(Engine::OpenMoss, "vulkan", &zen2).is_ok());
    assert!(resolve(Engine::AceStep, "cuda", &zen2).is_ok());
    let rows = installable(&inventory(&zen2), &zen2, &[]);
    let row = rows
        .iter()
        .find(|row| row.engine == Engine::OpenMoss && row.backend == "cuda")
        .unwrap();
    assert_eq!(
        row.availability,
        Some(crate::hardware::Availability::Unavailable)
    );
    assert_eq!(
        row.reasons.last().unwrap().code,
        crate::hardware::ReasonCode::CpuInstructionsMissing
    );
    let mut unknown = report(&rtx_host());
    unknown.devices.cpu = None;
    assert!(resolve(Engine::OpenMoss, "cuda", &unknown).is_ok());
}
