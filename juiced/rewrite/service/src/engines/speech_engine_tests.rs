use super::*;
use crate::{
    artifacts::ArtifactRole,
    config::Provider,
    hardware::tests::{cpu_only_host, report, rtx_host},
};
use catalog::{ArchiveFormat, Unresolved, resolve};
use models::{CompanionError, FormatError, ModelFormat};
use std::{io::Write, path::PathBuf};

fn inventory(host: &HostReport) -> Inventory {
    crate::hardware::project(&crate::catalog::Catalog::from_seed().unwrap(), host)
}

fn package(id: &str, engine: Engine, tag: &str) -> Installed {
    Installed {
        package: id.into(),
        engine: Some(engine),
        backend: Some("cpu".into()),
        gpu_target: None,
        tag: Some(tag.into()),
        installed: true,
        declared_host_match: true,
    }
}

fn written(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(bytes).unwrap();
    file
}

fn gguf_with_architecture(architecture: &str) -> Vec<u8> {
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
    bytes
}

#[test]
fn parakeet_and_moonshine_resolve_pinned_cpu_releases_and_futo_is_unpublished() {
    let host = report(&cpu_only_host());
    let parakeet = resolve(Engine::Parakeet, "cpu", &host).unwrap();
    assert_eq!(parakeet.repository, "mudler/parakeet.cpp");
    assert_eq!(parakeet.tag, "v0.6.1");
    assert_eq!(parakeet.asset, "parakeet-v0.6.1-bin-linux-cpu-x64.tar.gz");
    assert_eq!(parakeet.archive_bytes, 2727511);
    assert_eq!(
        parakeet.sha256,
        Some("cce60d122ab72e1068cd0d164e54a21655a0b83f1b9c21befc20124f5a972c10")
    );
    let moonshine = resolve(Engine::Moonshine, "cpu", &host).unwrap();
    assert_eq!(moonshine.repository, "lemonade-sdk/moonshine-server-rocm");
    assert_eq!(moonshine.tag, "moonshine0.0.62");
    assert_eq!(moonshine.format, ArchiveFormat::TarGz);
    assert_eq!(
        moonshine.sha256,
        Some("c19d6f333002af3209ee029be0e4f5967926edf29395de7401c83b5f9fc21191")
    );
    assert_eq!(
        resolve(Engine::Asr4all, "cpu", &host).unwrap_err(),
        Unresolved::NotPublished
    );
    assert_eq!(
        resolve(Engine::Parakeet, "cuda", &host).unwrap_err(),
        Unresolved::BackendUnknown
    );
    let mut aarch64 = host.clone();
    aarch64.host.architecture = "aarch64";
    for engine in [Engine::Parakeet, Engine::Moonshine] {
        assert_eq!(
            resolve(engine, "cpu", &aarch64).unwrap_err(),
            Unresolved::HostUnsupported
        );
    }
    for (name, engine) in [
        ("parakeet.cpp", Engine::Parakeet),
        ("parakeet", Engine::Parakeet),
        ("moonshine", Engine::Moonshine),
        ("futo-transcribe.cpp", Engine::Asr4all),
        ("asr4all", Engine::Asr4all),
    ] {
        assert_eq!(Engine::parse(name), Some(engine));
        assert_eq!(
            Engine::parse(engine.provider().implementation_id().unwrap()),
            Some(engine)
        );
        assert_eq!(
            engine.provider().media_operation(),
            Some(crate::config::Operation::Transcription)
        );
    }
    assert_eq!(
        Provider::Parakeet.transcription_path(),
        "/v1/audio/transcriptions"
    );
    assert_eq!(Provider::Moonshine.transcription_path(), "/inference");
    assert_eq!(Provider::Asr4all.transcription_path(), "/inference");
    assert_eq!(Provider::WhisperCpp.transcription_path(), "/inference");
}

#[test]
fn installable_rows_list_the_unpublished_engine_and_its_imported_package() {
    let host = report(&cpu_only_host());
    let inventory = inventory(&host);
    let imported = package("package-futo", Engine::Asr4all, "26d75933");
    let rows = installable(&inventory, &host, &[imported]);
    let futo = rows
        .iter()
        .find(|row| row.engine == Engine::Asr4all && row.backend == "cpu")
        .unwrap();
    assert!(futo.build.is_none());
    assert_eq!(futo.unresolved, Some(Unresolved::NotPublished));
    assert_eq!(futo.installed_package.as_deref(), Some("package-futo"));
    let parakeet = rows
        .iter()
        .find(|row| row.engine == Engine::Parakeet && row.backend == "cpu")
        .unwrap();
    assert!(parakeet.build.is_some());
    assert_eq!(parakeet.installed_package, None);
    assert_eq!(
        parakeet.availability,
        Some(crate::hardware::Availability::Available)
    );
}

#[test]
fn experimental_only_speech_engines_are_chosen_without_changing_whisper_recommendations() {
    let host = report(&rtx_host());
    let inventory = inventory(&host);
    assert_eq!(inventory.recommendations["parakeet"], None);
    assert_eq!(
        inventory.recommendations["moonshine"].as_deref(),
        Some("cpu")
    );
    assert_eq!(
        inventory.recommendations["whispercpp"].as_deref(),
        Some("cpu")
    );
    let installed = [
        package("package-parakeet", Engine::Parakeet, "v0.6.1"),
        package("package-futo", Engine::Asr4all, "26d75933"),
        package("package-moonshine", Engine::Moonshine, "moonshine0.0.62"),
    ];
    let parakeet = choose(Engine::Parakeet, &inventory, &host, &installed, None).unwrap();
    assert_eq!(parakeet.backend, "cpu");
    assert_eq!(parakeet.package, "package-parakeet");
    assert!(!parakeet.gpu);
    assert!(
        parakeet.reason.contains("only available"),
        "{}",
        parakeet.reason
    );
    let futo = choose(Engine::Asr4all, &inventory, &host, &installed, None).unwrap();
    assert_eq!(futo.package, "package-futo");
    let moonshine = choose(Engine::Moonshine, &inventory, &host, &installed, None).unwrap();
    assert!(moonshine.reason.contains("recommended"));
    assert_eq!(
        choose(Engine::Parakeet, &inventory, &host, &[], None),
        Err(ChoiceError::NotInstalled {
            backend: "cpu".into(),
            gpu_target: None,
            installable: true,
        })
    );
    assert_eq!(
        choose(Engine::Asr4all, &inventory, &host, &[], None),
        Err(ChoiceError::NotInstalled {
            backend: "cpu".into(),
            gpu_target: None,
            installable: false,
        })
    );
    assert!(matches!(
        choose(
            Engine::WhisperCpp,
            &inventory,
            &host,
            &installed,
            Some("rocm")
        ),
        Err(ChoiceError::NotSelectable(_))
    ));
}

#[test]
fn speech_artifacts_are_detected_by_architecture_and_container() {
    for (architecture, expected) in [
        ("parakeet", ModelFormat::ParakeetGguf {}),
        ("asr4all", ModelFormat::Asr4allGguf {}),
    ] {
        let file = written(&gguf_with_architecture(architecture));
        let (format, _) = models::detect(file.path()).unwrap();
        assert_eq!(format, expected);
        assert!(format.standalone());
        assert_eq!(models::context_tokens(&format), None);
    }
    let (llama, _) = models::detect(written(&gguf_with_architecture("llama")).path()).unwrap();
    assert_eq!(llama.engine(), Engine::LlamaCpp);
    let mut ort = 0x14u32.to_le_bytes().to_vec();
    ort.extend(b"ORTM");
    ort.extend([0; 32]);
    let (format, bytes) = models::detect(written(&ort).path()).unwrap();
    assert_eq!(format, ModelFormat::MoonshineOrt { model_arch: None });
    assert_eq!(bytes, ort.len() as u64);
    assert_eq!(format.engine(), Engine::Moonshine);
    assert!(!format.standalone());
    let estimate = models::estimate(&format, 1000, None);
    assert_eq!(estimate.basis, "weights-plus-workspace");
    assert_eq!(
        estimate.resident_bytes,
        1000 + models::SPEECH_WORKSPACE_BYTES
    );
    for (config, arch) in [
        (r#"{"encoder_dim":320,"depth":6,"nheads":8}"#, 2),
        (r#"{"encoder_dim":620,"depth":10}"#, 4),
        (r#"{"encoder_dim":768,"depth":14}"#, 5),
    ] {
        assert_eq!(
            models::moonshine_arch(written(config.as_bytes()).path()),
            Ok(arch)
        );
    }
    assert_eq!(
        models::moonshine_arch(written(br#"{"encoder_dim":512,"depth":8}"#).path()),
        Err(FormatError::Invalid("moonshine_architecture_unknown"))
    );
    assert_eq!(
        models::moonshine_arch(written(b"[]").path()),
        Err(FormatError::Invalid("invalid_moonshine_config"))
    );
    assert_eq!(
        models::moonshine_arch(written(&vec![b' '; 70 * 1024]).path()),
        Err(FormatError::Invalid("moonshine_config_limit"))
    );
    assert_eq!(
        models::native_default_id(&ModelFormat::MoonshineOrt {
            model_arch: Some(2)
        })
        .as_deref(),
        Some("Moonshine-Tiny-Streaming")
    );
}

fn moonshine_members() -> Vec<models::Companion> {
    [
        ("adapter.ort", ArtifactRole::Encoder),
        ("cross_kv.ort", ArtifactRole::Decoder),
        ("decoder.ort", ArtifactRole::Decoder),
        ("decoder_kv.ort", ArtifactRole::Decoder),
        ("frontend.ort", ArtifactRole::Encoder),
        ("ten-vad.onnx", ArtifactRole::Processor),
        ("streaming_config.json", ArtifactRole::Configuration),
        ("tokenizer.json", ArtifactRole::Tokenizer),
    ]
    .into_iter()
    .map(|(name, role)| models::Companion {
        name: name.into(),
        role,
    })
    .collect()
}

#[test]
fn speech_registrations_use_native_defaults_and_pass_the_argument_grammar() {
    let package = format!("package-{}", "c".repeat(64));
    let model = PathBuf::from("weights");
    let members = moonshine_members();
    for (format, threads, expected) in [
        (
            ModelFormat::ParakeetGguf {},
            Some(16),
            vec![
                "--model",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--threads",
                "4",
                "--concurrency",
                "1",
            ],
        ),
        (
            ModelFormat::ParakeetGguf {},
            Some(2),
            vec![
                "--model",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--threads",
                "2",
                "--concurrency",
                "1",
            ],
        ),
        (
            ModelFormat::Asr4allGguf {},
            None,
            vec![
                "--model",
                "{model}",
                "--port",
                "{port}",
                "--threads",
                "4",
                "--futo-commit",
                "256",
                "--futo-right",
                "32",
            ],
        ),
        (
            ModelFormat::MoonshineOrt {
                model_arch: Some(4),
            },
            Some(8),
            vec![
                "--model-path",
                "{model}",
                "--model-arch",
                "4",
                "--port",
                "{port}",
            ],
        ),
    ] {
        let companions: &[models::Companion] = if matches!(format, ModelFormat::MoonshineOrt { .. })
        {
            &members
        } else {
            &[]
        };
        let arguments = models::companion_arguments(&format, companions).unwrap();
        assert!(arguments.is_empty());
        let estimate = models::estimate(&format, 200_000_000, None);
        let registration = models::registration(&models::ProfileInputs {
            id: "speech",
            model_file: &model,
            artifact: None,
            artifact_set: None,
            package: &package,
            backend: "cpu",
            gpu: false,
            cpu_threads: threads,
            format: &format,
            estimate: &estimate,
            companion_arguments: &arguments,
        });
        let crate::config::Registration(spec) = serde_json::from_value(registration).unwrap();
        assert_eq!(spec.operations, [crate::config::Operation::Transcription]);
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
    let moonshine = ModelFormat::MoonshineOrt {
        model_arch: Some(2),
    };
    assert_eq!(
        models::companion_arguments(&moonshine, &members[..6]).unwrap_err(),
        CompanionError::Required("moonshine_companions_required")
    );
    let mut voice = members.clone();
    voice[0].role = ArtifactRole::Voice;
    assert!(models::companion_arguments(&moonshine, &voice).is_err());
    assert_eq!(
        models::companion_arguments(
            &ModelFormat::ParakeetGguf {},
            &[models::Companion {
                name: "extra.gguf".into(),
                role: ArtifactRole::Encoder,
            }]
        )
        .unwrap_err(),
        CompanionError::Unsupported
    );
}

#[test]
fn speech_worker_arguments_reject_unmanaged_ports_files_and_hosts() {
    use crate::runtime_arguments::validate;
    let args = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    };
    let parakeet = ["--model", "{model}", "--port", "{port}"];
    let template = validate(Provider::Parakeet, &args(&parakeet)).unwrap();
    assert!(
        template
            .arguments()
            .windows(2)
            .any(|pair| pair == ["--host", "127.0.0.1"])
    );
    for extra in [
        ["--cache-dir", "/tmp/models"],
        ["--sound-model", "/tmp/ced.gguf"],
        ["--host", "0.0.0.0"],
        ["--model", "/tmp/other.gguf"],
        ["--unknown", "1"],
    ] {
        let mut input = args(&parakeet);
        input.extend(args(&extra));
        assert!(validate(Provider::Parakeet, &input).is_err(), "{extra:?}");
    }
    let futo = ["--model", "{model}", "--port", "{port}", "-t", "2"];
    let template = validate(Provider::Asr4all, &args(&futo)).unwrap();
    assert!(!template.arguments().iter().any(|arg| arg == "--host"));
    let mut hosted = args(&futo);
    hosted.extend(args(&["--host", "127.0.0.1"]));
    assert!(validate(Provider::Asr4all, &hosted).is_err());
    let moonshine = [
        "--model-path",
        "{model}",
        "--port",
        "{port}",
        "--model-arch",
        "5",
    ];
    let template = validate(Provider::Moonshine, &args(&moonshine)).unwrap();
    assert_eq!(template.arguments(), args(&moonshine).as_slice());
    for extra in [
        ["--ws-port", "9000"],
        ["--tcp-port", "9001"],
        ["--model", "x"],
    ] {
        let mut input = args(&moonshine);
        input.extend(args(&extra));
        assert!(validate(Provider::Moonshine, &input).is_err(), "{extra:?}");
    }
    assert!(validate(Provider::Moonshine, &args(&["--port", "{port}"])).is_err());
}
