use super::*;
use crate::config::Config;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

struct Fixture {
    directory: tempfile::TempDir,
    source: tempfile::TempDir,
    configuration: PathBuf,
    input: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                            writable(&entry.path());
                        }
                    }
                }
            }
        }
        writable(self.directory.path());
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let service = directory.path().join("service");
    fs::create_dir(&service).unwrap();
    let configuration = service.join("config.json");
    let config:Config=serde_json::from_value(json!({"schema_version":1,"inhibit_suspend":false,"resident_budget_bytes":1024,"startup_timeout_ms":1000,"request_timeout_ms":1000,"models":[]})).unwrap();
    config.persist(&configuration).unwrap();
    fs::create_dir(source.path().join("lib")).unwrap();
    fs::write(source.path().join("engine"), b"fixture executable").unwrap();
    fs::write(source.path().join("lib/runtime"), b"fixture library").unwrap();
    let request = json!({"schema_version":1,"bundle":{"schema_version":1,"implementation_id":"llama-cpp","root":source.path(),"entrypoint":"engine","library_directories":["lib"],"files":[
        {"path":"lib/runtime","sha256":hash(b"fixture library"),"role":"library"},{"path":"engine","sha256":hash(b"fixture executable"),"role":"executable"}]},
        "provenance":{"builder_id":"native-fixture","source_revision":"synthetic-source","build_revision":"synthetic-build","declared_licenses":["unknown"],"declared_sbom":null},
        "declared_target":{"operating_system":std::env::consts::OS,"architecture":std::env::consts::ARCH,"abi":"unknown","compute_paths":["cpu"]},
        "files":[{"path":"lib/runtime","bytes":15},{"path":"engine","bytes":18}]});
    let input = directory.path().join("import.json");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    Fixture {
        directory,
        source,
        configuration,
        input,
    }
}
fn run(fixture: &Fixture) -> Result<ImportReceipt> {
    import_file(
        fixture.configuration.clone(),
        fixture.input.clone(),
        Arc::new(AtomicBool::new(false)),
    )
}
fn manifest(fixture: &Fixture) -> PathBuf {
    fixture
        .configuration
        .parent()
        .unwrap()
        .join(STORE)
        .join(MANIFEST)
}
fn spec(fixture: &Fixture) -> ImportRequest {
    let mut request: ImportRequest =
        serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
    request.canonicalize();
    request
}
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else if entry.file_type().unwrap().is_file() {
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    result
}

#[test]
fn local_runtime_install_publishes_the_exact_sealed_closure_and_retries_without_source_or_writes() {
    let fixture = fixture();
    let source_before = files(fixture.source.path());
    let config = fs::read(&fixture.configuration).unwrap();
    let receipt = run(&fixture).unwrap();
    assert!(receipt.installed && receipt.newly_installed);
    assert_eq!(receipt.reserved_bytes, 33);
    assert_eq!(receipt.compatibility, "unqualified");
    assert_eq!(files(&receipt.bundle.root), source_before);
    assert_eq!(files(fixture.source.path()), source_before);
    assert_eq!(fs::read(&fixture.configuration).unwrap(), config);
    let saved = fs::read(manifest(&fixture)).unwrap();
    let state: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(state["packages"][0]["phase"], "installed");
    assert_eq!(state["generation"], 2);
    assert_eq!(state["packages"][0]["specification"]["bundle"]["root"], ".");
    fs::remove_file(fixture.source.path().join("engine")).unwrap();
    let repeated = run(&fixture).unwrap();
    assert!(!repeated.newly_installed);
    assert_eq!(repeated.package, receipt.package);
    assert_eq!(repeated.bundle_digest, receipt.bundle_digest);
    assert_eq!(fs::read(manifest(&fixture)).unwrap(), saved);
    assert_eq!(files(&receipt.bundle.root), source_before);
}

#[test]
fn runtime_install_preserves_declared_empty_library_directories_and_checks_their_seal() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    fs::create_dir(fixture.source.path().join("optional-plugins")).unwrap();
    let mut request: Value = serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
    request["bundle"]["library_directories"]
        .as_array_mut()
        .unwrap()
        .push(json!("optional-plugins"));
    fs::write(&fixture.input, serde_json::to_vec(&request).unwrap()).unwrap();
    let receipt = run(&fixture).unwrap();
    let library = receipt.bundle.root.join("optional-plugins");
    assert!(library.is_dir());
    assert_eq!(
        fs::metadata(&library).unwrap().permissions().mode() & 0o7777,
        0o500
    );
    assert!(!run(&fixture).unwrap().newly_installed);
    fs::set_permissions(&library, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        run(&fixture).unwrap_err().reason(),
        "payload_ownership_changed"
    );
    assert_eq!(
        fs::metadata(library).unwrap().permissions().mode() & 0o7777,
        0o700
    );
}

#[test]
fn interrupted_runtime_install_keeps_reservations_and_matching_complete_files_resume_after_restart()
{
    for point in [Point::Copied(0), Point::Sealed] {
        let fixture = fixture();
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let error = import_checked(
            fixture.configuration.clone(),
            fixture.input.clone(),
            cancelled,
            |observed| {
                if observed == point {
                    token.store(true, Ordering::Release);
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.reason(), "cancelled");
        let state: Value = serde_json::from_slice(&fs::read(manifest(&fixture)).unwrap()).unwrap();
        assert_eq!(state["generation"], 1);
        assert_eq!(state["packages"][0]["phase"], "staging");
        assert_eq!(state["packages"][0]["reserved_bytes"], 33);
        let id = state["packages"][0]["id"].as_str().unwrap();
        let output = manifest(&fixture)
            .parent()
            .unwrap()
            .join(id)
            .join("payload/engine");
        assert_eq!(fs::read(output).unwrap(), b"fixture executable");
        let resumed = run(&fixture).unwrap();
        assert!(resumed.installed && resumed.newly_installed);
        assert_eq!(
            resumed.reused_files,
            if point == Point::Sealed { 2 } else { 1 }
        );
        assert_eq!(files(&resumed.bundle.root), files(fixture.source.path()));
    }
}

#[test]
fn cancellation_during_a_member_write_retains_the_full_reservation_and_refuses_partial_overwrite() {
    let fixture = fixture();
    let bytes = vec![23u8; 180000];
    fs::write(fixture.source.path().join("engine"), &bytes).unwrap();
    let mut request: Value = serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
    request["bundle"]["files"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|file| file["path"] == "engine")
        .unwrap()["sha256"] = json!(hash(&bytes));
    request["files"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|file| file["path"] == "engine")
        .unwrap()["bytes"] = json!(bytes.len());
    fs::write(&fixture.input, serde_json::to_vec(&request).unwrap()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let token = cancelled.clone();
    let mut chunks = 0;
    let error = import_checked(
        fixture.configuration.clone(),
        fixture.input.clone(),
        cancelled,
        |point| {
            if point == Point::Copying(0) {
                chunks += 1;
                if chunks == 3 {
                    token.store(true, Ordering::Release);
                }
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.reason(), "cancelled");
    let saved = fs::read(manifest(&fixture)).unwrap();
    let state: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(state["packages"][0]["phase"], "staging");
    assert_eq!(state["packages"][0]["reserved_bytes"], 180015);
    let id = state["packages"][0]["id"].as_str().unwrap();
    let output = manifest(&fixture)
        .parent()
        .unwrap()
        .join(id)
        .join("payload/engine");
    let retained = fs::read(&output).unwrap();
    assert_eq!(retained.len(), 65536);
    assert_eq!(retained, &bytes[..65536]);
    assert!(run(&fixture).is_err());
    assert_eq!(fs::read(&output).unwrap(), retained);
    assert_eq!(fs::read(manifest(&fixture)).unwrap(), saved);
}

#[test]
fn damaged_installed_runtime_is_never_repaired_or_republished_by_a_retry() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let saved = fs::read(manifest(&fixture)).unwrap();
    let engine = receipt.bundle.root.join("engine");
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&engine, b"changed executable").unwrap();
    assert_eq!(run(&fixture).unwrap_err().reason(), "payload_verification");
    assert_eq!(fs::read(&engine).unwrap(), b"changed executable");
    assert_eq!(fs::read(manifest(&fixture)).unwrap(), saved);
}

#[test]
fn post_seal_changes_refuse_installation_and_preserve_staging_without_repair() {
    use std::os::unix::fs::PermissionsExt;
    for change in ["content", "unexpected", "permissions", "declaration"] {
        let fixture = fixture();
        let original_config = fs::read(&fixture.configuration).unwrap();
        let original_source = files(fixture.source.path());
        let mut retained = None;
        let error = import_checked(
            fixture.configuration.clone(),
            fixture.input.clone(),
            Arc::new(AtomicBool::new(false)),
            |point| {
                if point != Point::Sealed {
                    return;
                }
                let head = fs::read(manifest(&fixture)).unwrap();
                let state: Value = serde_json::from_slice(&head).unwrap();
                let root = manifest(&fixture)
                    .parent()
                    .unwrap()
                    .join(state["packages"][0]["id"].as_str().unwrap())
                    .join("payload");
                match change {
                    "content" => {
                        let engine = root.join("engine");
                        fs::set_permissions(&engine, fs::Permissions::from_mode(0o600)).unwrap();
                        fs::write(&engine, b"changed executable").unwrap();
                        fs::set_permissions(engine, fs::Permissions::from_mode(0o500)).unwrap();
                    }
                    "unexpected" => {
                        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                        fs::write(root.join("unexpected"), b"preserve this unindexed file")
                            .unwrap();
                        fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
                    }
                    "permissions" => {
                        fs::set_permissions(root.join("engine"), fs::Permissions::from_mode(0o700))
                            .unwrap()
                    }
                    "declaration" => fs::write(&fixture.input, b"changed declaration\n").unwrap(),
                    _ => unreachable!(),
                }
                retained = Some((
                    head,
                    root.clone(),
                    files(&root),
                    fs::read(&fixture.input).unwrap(),
                ));
            },
        )
        .unwrap_err();
        assert_eq!(
            error.reason(),
            match change {
                "content" | "unexpected" => "payload_verification",
                "permissions" => "payload_ownership_changed",
                "declaration" => "storage_unavailable",
                _ => unreachable!(),
            }
        );
        let (head, root, payload, input) = retained.expect("reached the sealed checkpoint");
        assert_eq!(fs::read(manifest(&fixture)).unwrap(), head);
        let state: Value = serde_json::from_slice(&head).unwrap();
        assert_eq!(state["packages"][0]["phase"], "staging");
        assert_eq!(state["packages"][0]["reserved_bytes"], 33);
        assert_eq!(files(&root), payload);
        assert_eq!(fs::read(&fixture.input).unwrap(), input);
        assert_eq!(fs::read(&fixture.configuration).unwrap(), original_config);
        assert_eq!(files(fixture.source.path()), original_source);
    }
}

#[test]
fn installed_runtime_revalidates_sealed_permissions_and_never_silently_reseals_changed_files() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let saved = fs::read(manifest(&fixture)).unwrap();
    let engine = receipt.bundle.root.join("engine");
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        run(&fixture).unwrap_err().reason(),
        "payload_ownership_changed"
    );
    assert_eq!(
        fs::metadata(&engine).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(fs::read(manifest(&fixture)).unwrap(), saved);
}

#[test]
fn corrupt_duplicate_and_unindexed_package_metadata_refuse_mutation_without_empty_fallback() {
    for defect in ["missing-index", "duplicate", "foreign-entry"] {
        let fixture = fixture();
        let receipt = run(&fixture).unwrap();
        let mut state: Value =
            serde_json::from_slice(&fs::read(manifest(&fixture)).unwrap()).unwrap();
        match defect {
            "missing-index" => {
                state.as_object_mut().unwrap().remove("packages");
            }
            "duplicate" => {
                let duplicate = state["packages"][0].clone();
                state["packages"].as_array_mut().unwrap().push(duplicate);
            }
            _ => {
                fs::write(
                    manifest(&fixture).parent().unwrap().join("foreign"),
                    b"preserve",
                )
                .unwrap();
            }
        }
        if defect != "foreign-entry" {
            fs::write(manifest(&fixture), serde_json::to_vec(&state).unwrap()).unwrap();
        }
        let before = files(fixture.directory.path());
        assert!(run(&fixture).is_err(), "{defect}");
        assert_eq!(files(fixture.directory.path()), before);
        assert_eq!(
            fs::read(receipt.bundle.root.join("engine")).unwrap(),
            b"fixture executable"
        );
    }
}

#[test]
fn runtime_identity_is_order_and_location_independent_but_binds_roles_provenance_and_declared_target()
 {
    let fixture = fixture();
    let request = spec(&fixture);
    let id = request.identity().unwrap();
    let mut reordered = request.clone();
    reordered.bundle.root = PathBuf::from("/other/source");
    reordered.bundle.files.reverse();
    reordered.files.reverse();
    reordered.canonicalize();
    assert_eq!(reordered.identity().unwrap(), id);
    for field in ["role", "provenance", "target", "bytes"] {
        let mut changed = request.clone();
        match field {
            "role" => changed.bundle.files[1].role = FileRole::Resource,
            "provenance" => changed.provenance.build_revision = "different".into(),
            "target" => changed.declared_target.abi = "different".into(),
            _ => changed.files[0].bytes += 1,
        }
        assert_ne!(changed.identity().unwrap(), id, "{field}");
    }
}

#[test]
fn retained_staging_reservations_refuse_overcommit_before_hashing_or_creating_another_payload() {
    let fixture = fixture();
    let mut request = spec(&fixture);
    request.bundle.root = PathBuf::from(".");
    request.bundle.entrypoint = PathBuf::from("a");
    request.bundle.library_directories.clear();
    request.bundle.files.clear();
    request.files.clear();
    for name in ["a", "b", "c", "d"] {
        request
            .bundle
            .files
            .push(crate::runtime_bundle::BundleFile {
                path: PathBuf::from(name),
                sha256: hash(b"unmaterialized staged fixture"),
                role: if name == "a" {
                    FileRole::Executable
                } else {
                    FileRole::Library
                },
            });
        request.files.push(MemberBytes {
            path: PathBuf::from(name),
            bytes: crate::runtime_bundle::MAX_FILE_BYTES,
        });
    }
    request.canonicalize();
    assert_eq!(
        request.validate().unwrap(),
        crate::runtime_bundle::MAX_TOTAL_BYTES
    );
    let mut packages = Vec::new();
    for index in 0..4 {
        let mut reserved = request.clone();
        reserved.provenance.build_revision = format!("reserved-{index}");
        packages.push(Package {
            id: reserved.identity().unwrap(),
            specification: Arc::new(reserved.into()),
            phase: Phase::Staging,
            reserved_bytes: crate::runtime_bundle::MAX_TOTAL_BYTES,
        });
    }
    let state = Manifest {
        schema_version: 1,
        generation: 4,
        packages,
    };
    assert_eq!(state.validate().unwrap(), MAX_STORE_BYTES);
    {
        let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
        let bootstrap = LiveRoots::single(binding.root.clone()).unwrap();
        let owner = binding
            .root
            .acquire_private_child(&bootstrap, Path::new(STORE), true)
            .unwrap();
        let scope = LiveRoots::new(vec![binding.root, owner.clone()]).unwrap();
        scope.write_json(&owner, MANIFEST, &state).unwrap();
    }
    fs::remove_file(fixture.source.path().join("engine")).unwrap();
    let before = files(fixture.directory.path());
    assert_eq!(run(&fixture).unwrap_err().reason(), "store_limit");
    assert_eq!(files(fixture.directory.path()), before);
}

#[test]
fn runtime_package_root_replacement_preserves_the_foreign_tree_and_retained_staging_head() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    let root = fixture.configuration.parent().unwrap().join(STORE);
    let retained = root.with_file_name("retained-runtime-store");
    let error = import_checked(
        fixture.configuration.clone(),
        fixture.input.clone(),
        Arc::new(AtomicBool::new(false)),
        |point| {
            if point == Point::Reserved {
                fs::rename(&root, &retained).unwrap();
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                fs::write(root.join("foreign"), b"preserve").unwrap();
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.reason(), "storage_root_unavailable");
    assert_eq!(
        files(&root),
        BTreeMap::from([(PathBuf::from("foreign"), b"preserve".to_vec())])
    );
    let state: Value = serde_json::from_slice(&fs::read(retained.join(MANIFEST)).unwrap()).unwrap();
    assert_eq!(state["packages"][0]["phase"], "staging");
    assert_eq!(state["packages"][0]["reserved_bytes"], 33);
}

#[tokio::test]
async fn local_runtime_import_refuses_a_live_configuration_owner_before_creating_package_state() {
    let fixture = fixture();
    let manager = crate::supervisor::Manager::open(fixture.configuration.clone()).unwrap();
    let before = files(fixture.directory.path());
    assert_eq!(run(&fixture).unwrap_err().reason(), "owner_busy");
    assert_eq!(files(fixture.directory.path()), before);
    assert!(!manifest(&fixture).exists());
    assert!(manager.shutdown().await.is_settled());
    drop(manager);
}

fn serving_store(fixture: &Fixture) -> (ConfigurationBinding, Arc<super::managed::Store>) {
    let binding =
        crate::storage::after_test_release(fixture.configuration.parent().unwrap(), || {
            ConfigurationBinding::claim(fixture.configuration.clone())
        });
    let scope = LiveRoots::single(binding.root.clone()).unwrap();
    let store = super::managed::Store::open_optional(binding.root.clone(), scope)
        .unwrap()
        .unwrap();
    (binding, store)
}

fn protected_runtime_control(
    capacity: u64,
) -> (
    Arc<crate::admission::Admission>,
    Arc<crate::runtime_packages::memory::Controller>,
) {
    use crate::admission::{Admission, MemoryMode, MemorySettings, Settings};
    let admission = Admission::new(Settings {
        memory: MemorySettings {
            mode: MemoryMode::Protected,
            capacity_bytes: Some(capacity),
            essential_headroom_bytes: 0,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    admission.seed_host_sample_for_test(Some(1024 * 1024 * 1024));
    let control = crate::runtime_packages::memory::Controller::new(admission.clone());
    (admission, control)
}

#[test]
fn an_existing_package_root_without_its_control_file_is_never_reported_empty_or_repaired() {
    let fixture = fixture();
    run(&fixture).unwrap();
    fs::remove_file(
        fixture
            .configuration
            .parent()
            .unwrap()
            .join(STORE)
            .join(crate::storage::CONTROL_FILE),
    )
    .unwrap();
    let before = files(fixture.directory.path());
    {
        let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
        let scope = LiveRoots::single(binding.root.clone()).unwrap();
        assert_eq!(
            super::managed::Store::open_optional(binding.root.clone(), scope)
                .err()
                .unwrap()
                .reason(),
            "package_store_unavailable"
        );
    }
    assert_eq!(
        run(&fixture).unwrap_err().reason(),
        "package_store_unavailable"
    );
    assert_eq!(files(fixture.directory.path()), before);
}

#[test]
fn absent_runtime_index_does_not_reserve_metadata_or_create_files_in_a_small_protected_pool() {
    let fixture = fixture();
    let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
    let scope = LiveRoots::single(binding.root.clone()).unwrap();
    let (admission, control) = protected_runtime_control(1);
    let before = files(fixture.directory.path());
    assert!(
        super::managed::Store::open_optional_accounted(
            binding.root.clone(),
            scope,
            control.clone()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(control.projection()["metadata_bytes"], 0);
    assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
    assert_eq!(files(fixture.directory.path()), before);
}

#[test]
fn protected_index_bootstrap_refuses_unfunded_metadata_before_decoding_or_repairing_it() {
    let fixture = fixture();
    run(&fixture).unwrap();
    fs::write(manifest(&fixture), b"{invalid retained index").unwrap();
    let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
    let scope = LiveRoots::single(binding.root.clone()).unwrap();
    let (admission, control) = protected_runtime_control(32 * 1024 * 1024);
    let before = files(fixture.directory.path());
    let error = super::managed::Store::open_optional_accounted(
        binding.root.clone(),
        scope,
        control.clone(),
    )
    .err()
    .unwrap();
    assert_eq!(error.reason(), "host_memory_exhausted");
    assert_eq!(files(fixture.directory.path()), before);
    assert_eq!(control.projection()["metadata_bytes"], 0);
    assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
}

#[test]
fn import_metadata_bootstrap_refusal_preserves_absence_before_payload_source_inspection() {
    let fixture = fixture();
    fs::remove_file(fixture.source.path().join("engine")).unwrap();
    let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
    let scope = LiveRoots::single(binding.root.clone()).unwrap();
    let (admission, control) = protected_runtime_control(80 * 1024 * 1024);
    let work = control.import().unwrap();
    let before = files(fixture.directory.path());
    let slot = std::sync::OnceLock::new();
    let error = import_online(
        &slot,
        binding.root.clone(),
        scope,
        fixture.input.clone(),
        0,
        Arc::new(AtomicBool::new(false)),
        control.clone(),
    )
    .unwrap_err();
    assert_eq!(error.reason(), "host_memory_exhausted");
    assert!(slot.get().is_none());
    assert!(!manifest(&fixture).parent().unwrap().exists());
    assert_eq!(files(fixture.directory.path()), before);
    assert_eq!(control.projection()["metadata_bytes"], 0);
    drop(work);
    assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
}

#[test]
fn protected_runtime_metadata_and_binding_charges_follow_their_final_actual_owners() {
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
    let scope = LiveRoots::single(binding.root.clone()).unwrap();
    let (admission, control) = protected_runtime_control(128 * 1024 * 1024);
    let store = super::managed::Store::open_optional_accounted(
        binding.root.clone(),
        scope,
        control.clone(),
    )
    .unwrap()
    .unwrap();
    let head = fs::read(manifest(&fixture)).unwrap();
    let metadata = control.projection()["metadata_bytes"].as_u64().unwrap();
    assert_eq!(
        metadata,
        crate::runtime_packages::memory::Bounds::selected().metadata_bytes
    );
    let held = store
        .hold(
            &receipt.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    let copied = held.clone();
    let charges = control.projection();
    let retained = charges["binding_bytes"].as_u64().unwrap();
    assert!(retained > 0);
    assert_eq!(charges["binding_count"], 1);
    assert_eq!(charges["active_operations"], 0);
    assert_eq!(
        admission.memory_snapshot().unwrap().outstanding_bytes,
        metadata + retained
    );
    drop(store);
    drop(held);
    copied.verify().unwrap();
    assert_eq!(control.projection()["binding_count"], 1);
    assert_eq!(
        admission.memory_snapshot().unwrap().outstanding_bytes,
        metadata + retained
    );
    drop(copied);
    assert_eq!(control.projection()["binding_count"], 0);
    assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
    assert_eq!(fs::read(manifest(&fixture)).unwrap(), head);
}

#[test]
fn protected_binding_refusal_precedes_payload_inspection_and_refunds_its_attempt() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
    let scope = LiveRoots::single(binding.root.clone()).unwrap();
    let (admission, control) = protected_runtime_control(
        crate::runtime_packages::memory::Bounds::selected().bootstrap_bytes,
    );
    let store = super::managed::Store::open_optional_accounted(
        binding.root.clone(),
        scope,
        control.clone(),
    )
    .unwrap()
    .unwrap();
    fs::set_permissions(
        receipt.bundle.root.join("engine"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let before = files(fixture.directory.path());
    assert_eq!(
        store
            .hold(
                &receipt.package,
                crate::config::Provider::LlamaCpp,
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap_err()
            .reason(),
        "host_memory_exhausted"
    );
    assert_eq!(
        store.inventory().unwrap()["packages"][0]["binding_leases"],
        0
    );
    assert_eq!(control.projection()["binding_count"], 0);
    assert_eq!(control.projection()["active_operations"], 0);
    assert_eq!(
        admission.memory_snapshot().unwrap().outstanding_bytes,
        crate::runtime_packages::memory::Bounds::selected().metadata_bytes
    );
    assert_eq!(files(fixture.directory.path()), before);
}

#[test]
fn runtime_binding_counts_follow_distinct_leases_without_persisting_or_counting_arc_clones() {
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let head = fs::read(manifest(&fixture)).unwrap();
    let expected: Value = serde_json::from_slice(&head).unwrap();
    let decoded: Manifest = serde_json::from_slice(&head).unwrap();
    decoded.packages[0]
        .specification
        .binding_leases
        .store(3, Ordering::Release);
    assert_eq!(serde_json::to_value(&decoded).unwrap(), expected);
    let mut forged = expected.clone();
    forged["packages"][0]["specification"]["binding_leases"] = json!(0);
    assert!(serde_json::from_value::<Manifest>(forged).is_err());
    let (_binding, store) = serving_store(&fixture);
    let count = || {
        store.inventory().unwrap()["packages"][0]["binding_leases"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(count(), 0);
    let first = store
        .hold(
            &receipt.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    let clone = first.clone();
    assert_eq!(count(), 1);
    let second = store
        .hold(
            &receipt.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(count(), 2);
    drop(first);
    assert_eq!(count(), 2);
    drop(clone);
    assert_eq!(count(), 1);
    second.verify().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            receipt.bundle.root.join("engine"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert_eq!(
            store
                .hold(
                    &receipt.package,
                    crate::config::Provider::LlamaCpp,
                    Instant::now() + Duration::from_secs(2)
                )
                .unwrap_err()
                .reason(),
            "payload_ownership_changed"
        );
        assert_eq!(count(), 1);
    }
    drop(second);
    assert_eq!(count(), 0);
    assert_eq!(fs::read(manifest(&fixture)).unwrap(), head);
}

#[test]
fn cloned_managed_profiles_share_the_retained_bundle_and_keep_its_binding_after_original_drop() {
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let (_binding, store) = serving_store(&fixture);
    let mut profile: crate::execution_profile::ExecutionProfile = serde_json::from_value(json!({
        "schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp",
        "runtime_package":receipt.package,"args":["--host","127.0.0.1","--port","{port}","--model","{model}","--parallel","1"],"options":{},"placement":"unresolved"
    }))
    .unwrap();
    profile
        .bind_runtime_package(Some(&store), Instant::now() + Duration::from_secs(2))
        .unwrap();
    let cloned = profile.clone();
    assert!(std::ptr::eq(
        profile.runtime_bundle().unwrap(),
        cloned.runtime_bundle().unwrap()
    ));
    assert_eq!(
        store.inventory().unwrap()["packages"][0]["binding_leases"],
        1
    );
    let wire = serde_json::to_value(&cloned).unwrap();
    assert!(wire.get("runtime_bundle").is_none());
    assert!(wire.get("command").is_none());
    assert_eq!(wire["runtime_package"], receipt.package);
    drop(profile);
    cloned.package_binding().unwrap().verify().unwrap();
    assert_eq!(cloned.runtime_bundle().unwrap().root, receipt.bundle.root);
    assert_eq!(
        store.inventory().unwrap()["packages"][0]["binding_leases"],
        1
    );
    drop(cloned);
    assert_eq!(
        store.inventory().unwrap()["packages"][0]["binding_leases"],
        0
    );
}

#[test]
fn owned_index_publication_keeps_retained_bindings_responsive_and_installs_new_head_authority() {
    let fixture = fixture();
    let first = run(&fixture).unwrap();
    let (_binding, store) = serving_store(&fixture);
    let held = store
        .hold(
            &first.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let mut request: Value = serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
    request["provenance"]["build_revision"] = json!("live-second-record");
    fs::write(&fixture.input, serde_json::to_vec(&request).unwrap()).unwrap();
    let operation = Operation {
        deadline: Instant::now() + Duration::from_secs(10),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let input = prepare_input(fixture.input.clone(), &operation).unwrap();
    let mut probe = None;
    let mut child = None;
    let second = import_prepared(&store, input, &operation, Some(2), |point| {
        if point == Point::Publishing(3) {
            assert_eq!(
                store.inventory().unwrap_err().reason(),
                "runtime_index_publishing"
            );
            let held = held.clone();
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            child = Some(std::thread::spawn(move || {
                let _ = sender.send(held.verify());
            }));
            probe = Some(receiver.recv_timeout(Duration::from_secs(2)));
        }
    })
    .unwrap();
    child.unwrap().join().unwrap();
    probe
        .unwrap()
        .expect("retained binding verification must not wait for publication")
        .unwrap();
    held.verify().unwrap();
    let inventory = store.inventory().unwrap();
    assert_eq!(inventory["generation"], 4);
    assert_eq!(inventory["packages"].as_array().unwrap().len(), 2);
    assert_ne!(first.package, second.package);
    assert_eq!(inventory["packages"][0]["binding_leases"], 1);
    assert_eq!(inventory["packages"][1]["binding_leases"], 0);
    assert_eq!(files(&held.bundle().root), files(&second.bundle.root));
}

#[test]
fn index_publication_preserves_observed_foreign_head_changes_and_freezes_uncertain_authority() {
    let fixture = fixture();
    let first = run(&fixture).unwrap();
    let original_payload = files(&first.bundle.root);
    let (_binding, store) = serving_store(&fixture);
    let held = store
        .hold(
            &first.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let mut request: Value = serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
    request["provenance"]["build_revision"] = json!("foreign-head-refusal");
    fs::write(&fixture.input, serde_json::to_vec(&request).unwrap()).unwrap();
    let operation = Operation {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let input = prepare_input(fixture.input.clone(), &operation).unwrap();
    let mut reached = false;
    let error = import_prepared(&store, input, &operation, Some(2), |point| {
        if point == Point::Publishing(3) {
            reached = true;
            fs::write(manifest(&fixture), b"foreign head; preserve\n").unwrap();
            held.verify().unwrap();
        }
    })
    .unwrap_err();
    assert!(reached);
    assert_eq!(error.reason(), "storage_unavailable");
    assert_eq!(
        fs::read(manifest(&fixture)).unwrap(),
        b"foreign head; preserve\n"
    );
    assert!(held.verify().is_err());
    assert!(store.inventory().is_err());
    assert!(store.snapshot().is_err());
    assert_eq!(files(&first.bundle.root), original_payload);
}

#[test]
fn index_publication_refuses_replacing_immutable_records_or_dropping_existing_charges() {
    let fixture = fixture();
    run(&fixture).unwrap();
    let head = fs::read(manifest(&fixture)).unwrap();
    let (_binding, store) = serving_store(&fixture);
    let operation = Operation {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    for replacement in [true, false] {
        let mut next = store.snapshot().unwrap();
        next.generation += 1;
        if replacement {
            next.packages[0].specification =
                Arc::new(Record::from(next.packages[0].specification.request.clone()));
        } else {
            next.packages.clear();
        }
        assert_eq!(
            store.publish(next, &operation, || {}).unwrap_err().reason(),
            "package_record_changed"
        );
        store.verify().unwrap();
        assert_eq!(fs::read(manifest(&fixture)).unwrap(), head);
    }
}

#[test]
fn serving_package_owner_is_read_only_retains_exact_inputs_and_refuses_changed_manifest() {
    let fixture = fixture();
    let receipt = run(&fixture).unwrap();
    let before = files(fixture.configuration.parent().unwrap());
    let (_binding, store) = serving_store(&fixture);
    let held = store
        .hold(
            &receipt.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(held.id(), receipt.package);
    assert_eq!(held.bundle().root, receipt.bundle.root);
    assert_eq!(
        store.inventory().unwrap()["packages"][0]["phase"],
        "installed"
    );
    assert_eq!(files(fixture.configuration.parent().unwrap()), before);
    assert_eq!(
        store
            .hold(
                &receipt.package,
                crate::config::Provider::WhisperCpp,
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap_err()
            .reason(),
        "package_adapter_mismatch"
    );
    assert_eq!(
        store
            .hold(
                &format!("package-{}", "0".repeat(64)),
                crate::config::Provider::LlamaCpp,
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap_err()
            .reason(),
        "package_not_found"
    );
    fs::remove_file(fixture.source.path().join("engine")).unwrap();
    held.verify().unwrap();
    let saved = fs::read(manifest(&fixture)).unwrap();
    fs::write(manifest(&fixture), b"corrupt private manifest").unwrap();
    assert!(held.verify().is_err());
    assert!(store.inventory().is_err());
    assert_eq!(
        fs::read(manifest(&fixture)).unwrap(),
        b"corrupt private manifest"
    );
    assert_eq!(
        fs::read(held.bundle().root.join("engine")).unwrap(),
        b"fixture executable"
    );
    assert_ne!(saved, fs::read(manifest(&fixture)).unwrap());
}

#[test]
fn serving_package_owner_does_not_create_a_store_and_cannot_select_staging_payload() {
    let fixture = fixture();
    {
        let binding = ConfigurationBinding::claim(fixture.configuration.clone()).unwrap();
        let scope = LiveRoots::single(binding.root.clone()).unwrap();
        assert!(
            super::managed::Store::open_optional(binding.root.clone(), scope)
                .unwrap()
                .is_none()
        );
        assert!(!fixture.configuration.parent().unwrap().join(STORE).exists());
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    assert!(
        import_checked(
            fixture.configuration.clone(),
            fixture.input.clone(),
            cancelled.clone(),
            |point| {
                if point == Point::Reserved {
                    cancelled.store(true, Ordering::Release);
                }
            }
        )
        .is_err()
    );
    let (_binding, store) = serving_store(&fixture);
    let inventory = store.inventory().unwrap();
    assert_eq!(inventory["packages"][0]["phase"], "staging");
    assert_eq!(
        store
            .hold(
                inventory["packages"][0]["id"].as_str().unwrap(),
                crate::config::Provider::LlamaCpp,
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap_err()
            .reason(),
        "package_incomplete"
    );
    assert!(
        !fixture
            .configuration
            .parent()
            .unwrap()
            .join(STORE)
            .join(inventory["packages"][0]["id"].as_str().unwrap())
            .exists()
    );
}

#[test]
fn declared_target_mismatch_is_recorded_but_cannot_gain_installed_execution_authority() {
    for field in ["operating_system", "architecture"] {
        let fixture = fixture();
        let mut request: Value =
            serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
        request["declared_target"][field] = json!("foreign-target");
        fs::write(&fixture.input, serde_json::to_vec(&request).unwrap()).unwrap();
        let installed = run(&fixture).unwrap();
        let before = files(fixture.configuration.parent().unwrap());
        let (_binding, store) = serving_store(&fixture);
        let record = &store.inventory().unwrap()["packages"][0];
        assert_eq!(record["declared_target"][field], "foreign-target");
        assert_eq!(record["declared_host_match"], false);
        assert_eq!(record["compatibility"], "unqualified");
        assert_eq!(
            store
                .hold(
                    &installed.package,
                    crate::config::Provider::LlamaCpp,
                    Instant::now() + Duration::from_secs(2)
                )
                .unwrap_err()
                .reason(),
            "package_target_mismatch"
        );
        assert_eq!(files(fixture.configuration.parent().unwrap()), before);
    }
}

#[test]
fn declared_target_aliases_compare_without_rewriting_recorded_labels_or_claiming_qualification() {
    let fixture = fixture();
    let original = run(&fixture).unwrap();
    let mut request: Value = serde_json::from_slice(&fs::read(&fixture.input).unwrap()).unwrap();
    let alias = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    request["declared_target"]["architecture"] = json!(alias);
    if std::env::consts::OS == "macos" {
        request["declared_target"]["operating_system"] = json!("darwin");
    }
    fs::write(&fixture.input, serde_json::to_vec(&request).unwrap()).unwrap();
    let aliased = run(&fixture).unwrap();
    if alias != std::env::consts::ARCH || std::env::consts::OS == "macos" {
        assert_ne!(original.package, aliased.package);
    }
    let (_binding, store) = serving_store(&fixture);
    let record = store.inventory().unwrap()["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == aliased.package)
        .unwrap()
        .clone();
    assert_eq!(record["declared_target"]["architecture"], alias);
    assert_eq!(record["declared_host_match"], true);
    assert_eq!(record["compatibility"], "unqualified");
    store
        .hold(
            &aliased.package,
            crate::config::Provider::LlamaCpp,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
        .verify()
        .unwrap();
}
