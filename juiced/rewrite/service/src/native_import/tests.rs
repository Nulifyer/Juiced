use super::*;
use std::fs;

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
            r#"{{"builtin.Qwen3-0.6B-GGUF":{{"ctx_size":4096,"llamacpp_args":"-ngl 20 --flash-attn on --rope-scale {MARKER}","llamacpp_backend":"cpu","pinned":true,"evict_idle_timeout":30,"unknown_option":"{MARKER}"}},"unrelated":"{MARKER}","builtin.Removed-Model":{{}}}}"#
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
        &definition.registered,
        qwen.saved.as_ref().unwrap(),
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
        .find(|row| row.key == "llamacpp_args" && row.state == "unsupported")
        .unwrap();
    assert_eq!(
        unsupported.value,
        Some(serde_json::json!(["--flash-attn", "--rope-scale"]))
    );

    let mine = candidate(&inspection, "user.MyModel");
    assert_eq!(mine.public_name, "user.MyModel");
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
        &definition.registered,
        mine.saved.as_ref().unwrap(),
    );
    assert_eq!(mapped.options.context_tokens, None);
    assert_eq!(mapped.backend.as_deref(), Some("vulkan"));

    let vision = candidate(&inspection, "user.Vision");
    let weights = inspection.find_weights(vision).unwrap();
    assert!(weights.path.ends_with("Vision-F16.gguf"));
    assert_eq!(weights.selection, "repository-name");
    assert!(inspection.find_companion("acme/Vision-GGUF:mmproj-f16.gguf"));
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
            ("mine".into(), "user.MyModel".into())
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
    let mapped = map_options(
        Some(Engine::LlamaCpp),
        "llamacpp",
        &inspection.config,
        &BTreeMap::new(),
        saved,
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
