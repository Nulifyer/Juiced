use super::*;
use crate::{engines::catalog::Engine, profiles::LaunchOptions};
use std::fs;

fn layers<'a>(
    definition: &'a BTreeMap<String, Box<RawValue>>,
    saved: &'a crate::native_settings::SavedOptions,
) -> Vec<Layer<'a>> {
    vec![
        Layer {
            scope: "registration",
            values: definition
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_ref()))
                .collect(),
        },
        Layer {
            scope: "saved",
            values: saved.iter().collect(),
        },
    ]
}

const MARKER: &str = "JUICED_SYNTHETIC_IMPORT_MARKER";
const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn cancelled() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            root: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(fixture.config()).unwrap();
        fixture
    }
    fn volume(&self) -> PathBuf {
        fs::canonicalize(self.root.path()).unwrap()
    }
    fn config(&self) -> PathBuf {
        self.root.path().join("config")
    }
    fn hub(&self) -> PathBuf {
        self.root.path().join("huggingface/hub")
    }
    fn roots(&self) -> Vec<PathBuf> {
        vec![self.volume()]
    }
    fn file(&self, name: &str, bytes: impl AsRef<[u8]>) {
        write(&self.config().join(name), bytes);
    }
    fn weights(&self, repo: &str, files: &[&str], with_ref: bool) {
        let directory = self
            .hub()
            .join(format!("models--{}", repo.replace('/', "--")));
        for file in files {
            write(
                &directory.join("snapshots").join(REVISION).join(file),
                b"weights",
            );
        }
        if with_ref {
            write(&directory.join("refs/main"), REVISION);
        }
    }
    fn open(&self) -> Result<Inspection, Refusal> {
        Inspection::open(&self.config(), None, &self.roots(), cancelled())
    }
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut pending = vec![self.root.path().to_owned()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.insert(path.clone(), fs::read(path).unwrap());
                }
            }
        }
        files
    }
}

fn good() -> Fixture {
    let fixture = Fixture::new();
    fixture.file(
        "config.json",
        format!(
            r#"{{"ctx_size":8192,"llamacpp":{{"backend":"vulkan","args":"-t 6","cpu_bin":"builtin","rocm_bin":"/opt/custom"}},"max_loaded_models":2,"port":13305,"mystery":"{MARKER}"}}"#
        ),
    );
    fixture.file(
        "recipe_options.json",
        format!(
            r#"{{"builtin.Qwen3-0.6B-GGUF":{{"ctx_size":4096,"llamacpp_args":"-ngl 20 --flash-attn on --lora {MARKER} --made-up --host 0.0.0.0 --chat-template-kwargs '{{\"enable_thinking\":false}}'","llamacpp_backend":"cpu","pinned":true,"evict_idle_timeout":30,"unknown_option":"{MARKER}"}},"unrelated":"{MARKER}","builtin.Removed-Model":{{}}}}"#
        ),
    );
    fixture.file(
        "execution_profiles.json",
        r#"{"builtin.Qwen3-0.6B-GGUF":{"selected":"llamacpp:cpu","profiles":{"llamacpp:cpu":{"id":"llamacpp:cpu","recipe":"llamacpp","options":{}}}}}"#,
    );
    fixture.file(
        "user_models.json",
        r#"{"MyModel":{"checkpoint":"acme/My-GGUF:Q8_0","recipe":"llamacpp","labels":["chat"],"recipe_options":{"ctx_size":-1}},"Vision":{"checkpoint":"acme/Vision-GGUF","mmproj":"acme/Vision-GGUF:mmproj-f16.gguf","recipe":"llamacpp"},"Npu":{"checkpoint":"amd/Npu","recipe":"ryzenai-llm"}}"#,
    );
    write(
        &fixture.config().join("juiced/state.json"),
        r#"{"version":1,"policies":{"user.MyModel":"on-demand","Qwen3-0.6B-GGUF":"disabled"},"snapshots":{}}"#,
    );
    write(
        &fixture.root.path().join("cache/aliases.json"),
        r#"{"chat":"Qwen3-0.6B-GGUF","mine":"user.MyModel"}"#,
    );
    fixture.weights(
        "unsloth/Qwen3-0.6B-GGUF",
        &["Qwen3-0.6B-Q4_0.gguf", "Qwen3-0.6B-Q8_0.gguf"],
        true,
    );
    fixture.weights("acme/My-GGUF", &["My-Q8_0.gguf", "My-Q4_K_M.gguf"], false);
    fixture.weights(
        "acme/Vision-GGUF",
        &["eagle-draft.gguf", "Vision-F16.gguf", "mmproj-f16.gguf"],
        true,
    );
    fixture
}

fn candidate<'a>(inspection: &'a Inspection, id: &str) -> &'a Candidate {
    inspection
        .candidates
        .iter()
        .find(|candidate| candidate.native_id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}

#[test]
fn roots_confine_requests_including_symlink_escapes() {
    let fixture = good();
    let outside = tempfile::tempdir().unwrap();
    let error = confine_directory(&fixture.config(), &[]).unwrap_err();
    assert_eq!(error.code, "import_roots_unconfigured");
    let error = confine_directory(Path::new("relative/config"), &fixture.roots()).unwrap_err();
    assert_eq!(error.code, "invalid_import_path");
    let error = confine_directory(outside.path(), &fixture.roots()).unwrap_err();
    assert_eq!(error.code, "import_path_outside_roots");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), fixture.root.path().join("escape")).unwrap();
        let error =
            confine_directory(&fixture.root.path().join("escape"), &fixture.roots()).unwrap_err();
        assert_eq!(error.code, "import_path_outside_roots");
    }
    let error =
        confine_directory(&fixture.root.path().join("missing"), &fixture.roots()).unwrap_err();
    assert_eq!(error.code, "import_path_unavailable");
    assert_eq!(
        confine_directory(&fixture.config(), &fixture.roots()).unwrap(),
        fs::canonicalize(fixture.config()).unwrap()
    );
}

#[test]
fn good_source_maps_models_options_policies_aliases_and_weights_without_writes() {
    let fixture = good();
    let before = fixture.snapshot();
    let inspection = fixture.open().unwrap();
    assert_eq!(
        inspection.weights_root,
        Some(fs::canonicalize(fixture.hub()).unwrap())
    );
    let ids: Vec<_> = inspection
        .candidates
        .iter()
        .map(|candidate| (candidate.native_id.as_str(), candidate.origin))
        .collect();
    assert!(ids.contains(&("builtin.Qwen3-0.6B-GGUF", Origin::Builtin)));
    assert!(ids.contains(&("builtin.Removed-Model", Origin::Unrecognized)));
    assert!(ids.contains(&("user.MyModel", Origin::User)));
    assert!(!ids.iter().any(|(id, _)| id.contains("unrelated")));

    let qwen = candidate(&inspection, "builtin.Qwen3-0.6B-GGUF");
    assert_eq!(qwen.public_name, "Qwen3-0.6B-GGUF");
    assert_eq!(qwen.selected_profile.as_deref(), Some("llamacpp:cpu"));
    assert_eq!(
        qwen.previous_policy,
        Some(("disabled", "native-juiced-state"))
    );
    let weights = inspection.find_weights(qwen).unwrap();
    assert!(weights.path.ends_with("Qwen3-0.6B-Q4_0.gguf"));
    assert_eq!((weights.bytes, weights.selection), (7, "variant"));
    let definition = qwen.definition.as_ref().unwrap();
    let mapped = map_options(
        Some(Engine::LlamaCpp),
        "llamacpp",
        &inspection.config,
        &layers(&definition.registered, qwen.saved.as_ref().unwrap()),
        &[],
    );
    assert_eq!(
        mapped.options,
        LaunchOptions {
            context_tokens: Some(4096),
            threads: Some(6),
            gpu_layers: Some(20),
            parallel_slots: None
        }
    );
    assert_eq!(mapped.backend.as_deref(), Some("cpu"));
    assert!(mapped.pinned);
    let state = |key: &str| {
        mapped
            .rows
            .iter()
            .find(|row| row.key == key)
            .map(|row| row.state)
    };
    assert_eq!(state("ctx_size"), Some("mapped"));
    assert_eq!(state("evict_idle_timeout"), Some("unsupported"));
    assert_eq!(state("unknown_option"), Some("unknown"));
    let unsupported = mapped
        .rows
        .iter()
        .find(|row| row.key == "llamacpp_args:unsupported")
        .unwrap();
    assert_eq!(
        unsupported.value,
        Some(serde_json::json!(["--lora", "--made-up"]))
    );
    let managed = mapped
        .rows
        .iter()
        .find(|row| row.key == "llamacpp_args:managed")
        .unwrap();
    assert_eq!(managed.value, Some(serde_json::json!(["--host"])));
    assert_eq!(
        mapped.arguments,
        vec![
            vec!["--flash-attn".to_owned(), "on".to_owned()],
            vec![
                "--chat-template-kwargs".to_owned(),
                r#"{"enable_thinking":false}"#.to_owned()
            ],
        ]
    );

    let mine = candidate(&inspection, "user.MyModel");
    assert_eq!(mine.public_name, "MyModel");
    assert_eq!(
        mine.previous_policy,
        Some(("on-demand", "native-juiced-state"))
    );
    let weights = inspection.find_weights(mine).unwrap();
    assert!(weights.path.ends_with("My-Q8_0.gguf"));
    let definition = mine.definition.as_ref().unwrap();
    let mapped = map_options(
        Some(Engine::LlamaCpp),
        "llamacpp",
        &inspection.config,
        &layers(&definition.registered, mine.saved.as_ref().unwrap()),
        &[],
    );
    assert_eq!(mapped.options.context_tokens, None);
    assert!(mapped.automatic_context);
    assert_eq!(mapped.backend.as_deref(), Some("vulkan"));

    let vision = candidate(&inspection, "user.Vision");
    let weights = inspection.find_weights(vision).unwrap();
    assert!(weights.path.ends_with("Vision-F16.gguf"));
    assert_eq!(weights.selection, "repository-name");
    let projector = inspection
        .find_checkpoint(
            vision
                .definition
                .as_ref()
                .unwrap()
                .mmproj
                .as_deref()
                .unwrap(),
            Some("acme/Vision-GGUF"),
        )
        .unwrap();
    assert!(projector.path.ends_with("mmproj-f16.gguf"));
    assert_eq!(
        inspection
            .find_weights(candidate(&inspection, "user.Npu"))
            .unwrap_err(),
        "weights_missing"
    );
    assert_eq!(
        inspection
            .find_weights(candidate(&inspection, "builtin.Removed-Model"))
            .unwrap_err(),
        "native_definition_missing"
    );

    assert_eq!(
        inspection.aliases,
        BTreeMap::from([
            ("chat".into(), "Qwen3-0.6B-GGUF".into()),
            ("mine".into(), "MyModel".into())
        ])
    );
    let global = global_rows(&inspection.config);
    let row = |key: &str| global.iter().find(|row| row.key == key).unwrap();
    assert_eq!(row("ctx_size").state, "mapped");
    assert_eq!(row("max_loaded_models").state, "unsupported");
    assert_eq!(row("port").state, "ignored");
    assert_eq!(row("mystery").state, "unknown");
    assert_eq!(row("mystery").value, None);
    assert_eq!(
        row("llamacpp.backend").value,
        Some(serde_json::json!("vulkan"))
    );
    assert_eq!(row("llamacpp.cpu_bin").state, "ignored");
    assert_eq!(row("llamacpp.rocm_bin").state, "unsupported");
    let report = serde_json::to_string(&(&global, &inspection.documents)).unwrap();
    assert!(!report.contains(MARKER), "{report}");
    assert!(!report.contains("/opt/custom"), "{report}");
    let damaged = serde_json::to_string(&mapped.rows).unwrap();
    assert!(!damaged.contains(MARKER));
    inspection.verify().unwrap();
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn damaged_documents_refuse_with_static_reasons_and_preserve_bytes() {
    for (file, bytes, expected) in [
        (
            "recipe_options.json",
            format!("{{\"{MARKER}\":"),
            "recipe_options.json cannot be imported: it is not valid JSON",
        ),
        (
            "recipe_options.json",
            format!("{{\"builtin.Qwen3-0.6B-GGUF\":\"{MARKER}\"}}"),
            "recipe_options.json cannot be imported: a known model row is not an object",
        ),
        (
            "user_models.json",
            format!("[\"{MARKER}\"]"),
            "user_models.json cannot be imported: its top level is not an object",
        ),
        (
            "execution_profiles.json",
            format!("{{\"a\":1,\"a\":\"{MARKER}\"}}"),
            "execution_profiles.json cannot be imported: it repeats a key",
        ),
        (
            "execution_profiles.json",
            format!(
                "{{\"builtin.Qwen3-0.6B-GGUF\":{{\"selected\":\"{MARKER}\",\"profiles\":{{}},\"settings\":{{\"version\":2,\"saved_options\":{{}}}}}}}}"
            ),
            "execution_profiles.json cannot be imported: a profile-owned settings row is incomplete or unsupported",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.file(file, &bytes);
        let before = fixture.snapshot();
        let error = fixture
            .open()
            .err()
            .unwrap_or_else(|| panic!("{file} accepted"));
        assert_eq!(error.code, "model_settings_recovery_required", "{file}");
        assert!(error.message.starts_with(expected), "{}", error.message);
        assert!(!error.message.contains(MARKER));
        assert!(!format!("{error:?}").contains(MARKER));
        assert_eq!(fixture.snapshot(), before);
    }
}

#[test]
fn profile_owned_settings_are_the_saved_authority() {
    let fixture = Fixture::new();
    fixture.file(
        "recipe_options.json",
        r#"{"builtin.Qwen3-0.6B-GGUF":{"ctx_size":9999}}"#,
    );
    fixture.file(
        "execution_profiles.json",
        r#"{"builtin.Qwen3-0.6B-GGUF":{"selected":"custom","profiles":{"custom":{"recipe":"llamacpp"}},"settings":{"version":1,"saved_options":{"ctx_size":2048,"future_option":{"nested":[1,2]}}}}}"#,
    );
    let inspection = fixture.open().unwrap();
    let qwen = candidate(&inspection, "builtin.Qwen3-0.6B-GGUF");
    assert_eq!(qwen.selected_profile.as_deref(), Some("custom"));
    let saved = qwen.saved.as_ref().unwrap();
    assert_eq!(saved.get("ctx_size").unwrap().get(), "2048");
    let empty = BTreeMap::new();
    let mapped = map_options(
        Some(Engine::LlamaCpp),
        "llamacpp",
        &inspection.config,
        &layers(&empty, saved),
        &[],
    );
    assert_eq!(mapped.options.context_tokens, Some(2048));
    assert!(
        mapped
            .rows
            .iter()
            .any(|row| row.key == "future_option" && row.state == "unknown")
    );
    assert_eq!(inspection.weights_root, None);
    assert_eq!(
        inspection.find_weights(qwen).unwrap_err(),
        "weights_root_missing"
    );
}

#[test]
fn native_state_store_is_tolerated_but_migration_markers_refuse() {
    let fixture = Fixture::new();
    fixture.file("recipe_options.json", "{}");
    fs::create_dir_all(fixture.config().join("juiced/snapshots")).unwrap();
    write(&fixture.config().join("juiced/state.json"), "not json");
    let inspection = fixture.open().unwrap();
    let state = inspection
        .documents
        .iter()
        .find(|document| document.file == "juiced/state.json")
        .unwrap();
    assert_eq!(state.state, DocumentState::Damaged);
    fixture.file(".juiced-configuration-migration.json", "{}");
    let error = fixture.open().err().unwrap();
    assert_eq!(error.code, "configuration_migration_recovery_required");
    let offline = SourceCohort::inspect(fixture.config(), false, cancelled())
        .err()
        .unwrap();
    assert_eq!(offline.code(), "configuration_migration_recovery_required");
}

#[test]
fn offline_cohort_still_refuses_a_native_state_store() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.config().join("juiced")).unwrap();
    assert!(SourceCohort::inspect(fixture.config(), false, cancelled()).is_err());
    assert!(SourceCohort::inspect_for_mapping(fixture.config(), cancelled()).is_ok());
}

#[test]
fn local_path_weights_must_stay_under_import_roots() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("model.gguf"), b"x");
    write(&fixture.root.path().join("models/local.gguf"), b"xy");
    fixture.file(
        "user_models.json",
        serde_json::json!({
            "Inside": {"checkpoint": fixture.root.path().join("models/local.gguf"), "recipe": "llamacpp", "source": "local_path"},
            "Outside": {"checkpoint": outside.path().join("model.gguf"), "recipe": "llamacpp", "source": "local_path"},
            "Upload": {"checkpoint": "../escape.gguf", "recipe": "llamacpp", "source": "local_upload"},
        })
        .to_string(),
    );
    fs::create_dir_all(fixture.hub()).unwrap();
    let inspection = fixture.open().unwrap();
    let inside = inspection
        .find_weights(candidate(&inspection, "user.Inside"))
        .unwrap();
    assert_eq!((inside.bytes, inside.selection), (2, "local-path"));
    assert_eq!(
        inspection
            .find_weights(candidate(&inspection, "user.Outside"))
            .unwrap_err(),
        "weights_outside_import_roots"
    );
    assert_eq!(
        inspection
            .find_weights(candidate(&inspection, "user.Upload"))
            .unwrap_err(),
        "weights_missing"
    );
}

#[test]
fn variant_selection_matches_native_rules() {
    let files: Vec<(String, PathBuf, u64)> = [
        ("Q4_0/model-Q4_0-00001-of-00002.gguf", 10),
        ("Q4_0/model-Q4_0-00002-of-00002.gguf", 11),
        ("model-IQ4_XS-Q8nextn.gguf", 3),
        ("model-Q8_0.gguf", 5),
        ("mmproj-model-f16.gguf", 2),
        ("README.md", 1),
    ]
    .into_iter()
    .map(|(name, bytes)| (name.to_owned(), PathBuf::from("/w").join(name), bytes))
    .collect();
    let chosen = select(&files, Some("Q4_0"), "model-GGUF").unwrap();
    assert_eq!(
        (chosen.selection, chosen.bytes, chosen.files.len()),
        ("variant-folder", 21, 2)
    );
    assert!(chosen.path.ends_with("model-Q4_0-00001-of-00002.gguf"));
    assert_eq!(select(&files, Some("q8_0"), "model-GGUF").unwrap().bytes, 5);
    assert_eq!(
        select(&files, Some("IQ4_XS"), "model-GGUF").unwrap().bytes,
        3
    );
    assert_eq!(
        select(&files, Some("model-Q8_0.gguf"), "x")
            .unwrap()
            .selection,
        "exact-file"
    );
    assert!(select(&files, Some("Q2_K"), "model-GGUF").is_none());
    let first = select(&files, None, "model-GGUF").unwrap();
    assert_eq!(first.selection, "repository-name");
    assert!(!first.path.to_string_lossy().contains("mmproj"));
}

fn raw(text: &str) -> Box<RawValue> {
    RawValue::from_string(text.to_owned()).unwrap()
}

fn one_layer<'a>(scope: &'static str, values: &'a [(&'a str, Box<RawValue>)]) -> Layer<'a> {
    Layer {
        scope,
        values: values
            .iter()
            .map(|(key, value)| (*key, value.as_ref()))
            .collect(),
    }
}

#[test]
fn custom_arguments_split_like_native() {
    assert_eq!(
        shell_words(r#"--temp 1.0  --chat-template-kwargs '{"a":true}' -x "b c""#),
        vec![
            "--temp",
            "1.0",
            "--chat-template-kwargs",
            r#"{"a":true}"#,
            "-x",
            "b c"
        ]
    );
    assert!(shell_words("   ").is_empty());
}

#[test]
fn image_defaults_layer_under_recipe_options_and_operator_arguments() {
    let defaults = [
        ("steps", raw("4")),
        ("width", raw("512")),
        ("flow_shift", raw("3.0")),
    ];
    let registered = [
        ("steps", raw("8")),
        ("cfg_scale", raw("1.0")),
        ("width", raw("768")),
        ("height", raw("768")),
        ("sampling_method", raw(r#""euler""#)),
        ("scheduler", raw(r#""karras""#)),
        ("sd-cpp_backend", raw(r#""vulkan""#)),
        (
            "sdcpp_args",
            raw(
                r#""--scheduler simple --listen-ip 127.0.0.1 --params-backend Vulkan0 -t 8 --diffusion-fa""#,
            ),
        ),
    ];
    let saved = [("height", raw("1024")), ("pinned", raw("false"))];
    let global = Document::parse_bytes(Role::Config, Some(b"{}")).unwrap();
    let mapped = map_options(
        Some(Engine::StableDiffusionCpp),
        "sd-cpp",
        &global,
        &[
            one_layer("image_defaults", &defaults),
            one_layer("registration", &registered),
            one_layer("saved", &saved),
        ],
        &[],
    );
    let flat: Vec<String> = mapped.arguments.concat();
    assert_eq!(
        flat,
        [
            "--scheduler",
            "simple",
            "-t",
            "8",
            "--diffusion-fa",
            "--cfg-scale",
            "1",
            "--flow-shift",
            "3",
            "-H",
            "1024",
            "--sampling-method",
            "euler",
            "--steps",
            "8",
            "-W",
            "768"
        ]
    );
    assert_eq!(mapped.backend.as_deref(), Some("vulkan"));
    let row = |key: &str| mapped.rows.iter().find(|row| row.key == key).unwrap();
    assert_eq!(
        row("sdcpp_args:managed").value,
        Some(serde_json::json!(["--listen-ip"]))
    );
    assert_eq!(
        row("sdcpp_args:unsupported").value,
        Some(serde_json::json!(["--params-backend"]))
    );
}

#[test]
fn speech_hybrid_and_mtp_options_map_to_engine_arguments() {
    let global = Document::parse_bytes(Role::Config, Some(b"{}")).unwrap();
    let speech = [
        ("asr4all_threads", raw(r#""4""#)),
        ("asr4all_commit", raw(r#""16""#)),
        ("asr4all_right", raw("4")),
    ];
    let mapped = map_options(
        Some(Engine::Asr4all),
        "asr4all",
        &global,
        &[one_layer("registration", &speech)],
        &[],
    );
    assert_eq!(
        mapped.arguments,
        vec![
            vec!["--futo-commit".to_owned(), "16".to_owned()],
            vec!["--futo-right".to_owned(), "4".to_owned()],
            vec!["--threads".to_owned(), "4".to_owned()],
        ]
    );
    let hybrid = [
        ("llamacpp_backend", raw(r#""hybrid-rocm""#)),
        ("llamacpp_device", raw(r#""ROCm0""#)),
        ("llamacpp_hybrid_memory_mib", raw("2048")),
        ("hybrid_copy_gib", raw("16")),
        ("ctx_size", raw("-1")),
        ("llamacpp_args", raw(r#""--parallel 2 -ngl 999 -ngl 99""#)),
    ];
    let labels = vec!["mtp".to_owned()];
    let mapped = map_options(
        Some(Engine::LlamaCpp),
        "llamacpp",
        &global,
        &[one_layer("saved", &hybrid)],
        &labels,
    );
    assert_eq!(mapped.backend.as_deref(), Some("hybrid-rocm"));
    assert!(mapped.automatic_context);
    assert_eq!(mapped.options.parallel_slots, Some(2));
    assert_eq!(mapped.options.gpu_layers, Some(99));
    let flat = mapped.arguments.concat();
    assert!(
        flat.windows(2)
            .any(|pair| pair == [crate::hybrid::DEVICE_FLAG, "ROCm0"])
    );
    assert!(
        flat.windows(2)
            .any(|pair| pair == [crate::hybrid::MEMORY_FLAG, "2048"])
    );
    assert!(
        flat.windows(2)
            .any(|pair| pair == ["--spec-type", "draft-mtp"])
    );
    assert!(!flat.iter().any(|argument| argument == "--device"));
    assert!(
        mapped
            .rows
            .iter()
            .any(|row| row.key == "hybrid_copy_gib" && row.state == "unsupported")
    );
    let plain = [
        ("merge_args", raw("false")),
        ("llamacpp_device", raw(r#""Vulkan0""#)),
    ];
    let mapped = map_options(
        Some(Engine::LlamaCpp),
        "llamacpp",
        &global,
        &[one_layer("saved", &plain)],
        &labels,
    );
    assert_eq!(
        mapped.arguments,
        vec![vec!["--device".to_owned(), "Vulkan0".to_owned()]]
    );
}

#[test]
fn native_names_resolve_across_hubs_snapshots_and_companion_roles() {
    let fixture = Fixture::new();
    fixture.file(
        "user_models.json",
        r#"{
          "Qwen3-0.6B-GGUF":{"checkpoint":"acme/Shadow-GGUF:Q4_0","recipe":"llamacpp"},
          "Old":{"checkpoints":{"main":"acme/Multi-GGUF:Q8_0","mmproj":"acme/Multi-GGUF:mmproj-F16.gguf"},"recipe":"llamacpp"},
          "Image":{"checkpoints":{"main":"acme/Image:diffusion/model.safetensors","text_encoder":"acme/Encoder-GGUF:enc-Q4_K_M.gguf","vae":"acme/Image:split_files/vae/ae.safetensors"},"recipe":"sd-cpp","image_defaults":{"steps":4}}
        }"#,
    );
    fixture.file(
        "recipe_options.json",
        r#"{"builtin.Qwen3-0.6B-GGUF":{},"builtin.kokoro-v1":{}}"#,
    );
    fixture.weights("acme/Shadow-GGUF", &["Shadow-Q4_0.gguf"], true);
    fixture.weights("unsloth/Qwen3-0.6B-GGUF", &["Qwen3-0.6B-Q4_0.gguf"], true);
    let multi = fixture.hub().join("models--acme--Multi-GGUF");
    write(&multi.join("snapshots/1111/Multi-Q8_0.gguf"), b"older");
    write(&multi.join("snapshots/2222/Multi-Q4_0.gguf"), b"newer");
    write(&multi.join("snapshots/3333/Multi-Q8_0.gguf"), b"partial");
    write(&multi.join("snapshots/3333/.download_manifest.json"), b"{}");
    write(&multi.join("snapshots/2222/mmproj-F16.gguf"), b"proj");
    write(&multi.join("refs/main"), "2222");
    let library = fixture.root.path().join("library/huggingface/hub");
    let image = library.join("models--acme--Image/snapshots/abcd");
    write(&image.join("diffusion/model.safetensors"), b"main");
    write(&image.join("split_files/vae/ae.safetensors"), b"vae");
    write(
        &fixture
            .hub()
            .join("models--acme--Encoder-GGUF/snapshots/beef/enc-Q4_K_M.gguf"),
        b"encoder",
    );
    let kokoro = fixture
        .hub()
        .join("models--mikkoph--kokoro-onnx/snapshots/cafe");
    write(
        &kokoro.join("index.json"),
        r#"{"model":"kokoro-v1.0.onnx","voices":"voices-v1.0.bin"}"#,
    );
    write(&kokoro.join("kokoro-v1.0.onnx"), b"onnx");
    write(&kokoro.join("voices-v1.0.bin"), b"voices");
    let roots = vec![
        fixture.volume(),
        fs::canonicalize(fixture.root.path().join("library")).unwrap(),
    ];
    let inspection = Inspection::open(&fixture.config(), None, &roots, cancelled()).unwrap();
    assert_eq!(inspection.weights_roots.len(), 2);

    let shadowed = candidate(&inspection, "builtin.Qwen3-0.6B-GGUF");
    assert_eq!(shadowed.public_name, "builtin.Qwen3-0.6B-GGUF");
    let user = candidate(&inspection, "user.Qwen3-0.6B-GGUF");
    assert_eq!(user.public_name, "Qwen3-0.6B-GGUF");
    assert!(
        inspection
            .find_weights(user)
            .unwrap()
            .path
            .ends_with("Shadow-Q4_0.gguf")
    );

    let old = candidate(&inspection, "user.Old");
    let found = inspection.find_weights(old).unwrap();
    assert_eq!(found.snapshot.as_deref(), Some("1111"));
    assert_eq!(found.bytes, 5);
    let projector = inspection
        .find_checkpoint(
            &old.definition.as_ref().unwrap().checkpoints["mmproj"],
            None,
        )
        .unwrap();
    assert_eq!(projector.snapshot.as_deref(), Some("2222"));

    let image = candidate(&inspection, "user.Image");
    let definition = image.definition.as_ref().unwrap();
    assert!(
        inspection
            .find_weights(image)
            .unwrap()
            .path
            .ends_with("diffusion/model.safetensors")
    );
    let main = definition.checkpoint.as_deref();
    let vae = inspection
        .find_checkpoint(&definition.checkpoints["vae"], main)
        .unwrap();
    assert!(vae.path.starts_with(fs::canonicalize(&library).unwrap()));
    assert!(
        inspection
            .find_checkpoint(&definition.checkpoints["text_encoder"], main)
            .unwrap()
            .path
            .ends_with("enc-Q4_K_M.gguf")
    );
    assert_eq!(definition.image_defaults["steps"].get(), "4");
    assert_eq!(
        inspection
            .find_checkpoint("acme/Image:missing.safetensors", main)
            .unwrap_err(),
        "weights_missing"
    );

    let kokoro = candidate(&inspection, "builtin.kokoro-v1");
    assert_eq!(kokoro.public_name, "kokoro-v1");
    let (model, voices) = inspection.find_kokoro(kokoro).unwrap();
    assert!(model.path.ends_with("kokoro-v1.0.onnx"));
    assert!(voices.ends_with("voices-v1.0.bin"));
}
