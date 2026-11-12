use super::tests::{settled, store};
use super::*;
use serde_json::{Value, json};

const MODEL_BYTES: &[u8] = b"selected-model-fixture";
const COMPANION_BYTES: &[u8] = b"selected-projector-fixture";

fn operation() -> SetOperation {
    SetOperation::new(
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    )
}
async fn import(store: &Arc<ArtifactStore>, bytes: &[u8]) -> String {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let job = store
        .start_import(
            file,
            ImportRequest {
                expected_sha256: digest.clone(),
                expected_bytes: Some(bytes.len() as u64),
                maximum_bytes: bytes.len() as u64,
            },
            store.generation(),
        )
        .unwrap();
    let complete = settled(store, &job.id).await;
    assert_eq!(complete.state, JobState::Complete, "{complete:?}");
    digest
}
async fn specification(store: &Arc<ArtifactStore>) -> ArtifactSetSpec {
    let model = import(store, MODEL_BYTES).await;
    let companion = import(store, COMPANION_BYTES).await;
    ArtifactSetSpec {
        schema_version: 1,
        origin: ArtifactSetOrigin {
            importer_id: "fixture-importer".into(),
            importer_version: "1".into(),
            source_revision: "pinned-fixture-v1".into(),
        },
        primary: "model".into(),
        members: vec![
            ArtifactSetMember {
                name: "projector".into(),
                role: ArtifactRole::Projector,
                artifact: companion,
                bytes: COMPANION_BYTES.len() as u64,
            },
            ArtifactSetMember {
                name: "model".into(),
                role: ArtifactRole::Model,
                artifact: model,
                bytes: MODEL_BYTES.len() as u64,
            },
        ],
    }
}
fn durable(store: &ArtifactStore) -> Vec<u8> {
    fs::read(store.lease.root().join(MANIFEST)).unwrap()
}
fn assert_no_holds(store: &ArtifactStore) {
    assert!(
        store
            .inspect()
            .unwrap()
            .artifacts
            .iter()
            .all(|artifact| artifact.active_holds == 0)
    );
    assert!(
        store
            .inspect()
            .unwrap()
            .artifact_sets
            .iter()
            .all(|set| set.active_holds == 0)
    );
}

#[tokio::test]
async fn model_revision_references_update_whole_bindings_once_and_deduplicate_shared_content() {
    let (_directory, store) = store();
    let mut spec = specification(&store).await;
    spec.members
        .retain(|member| member.role == ArtifactRole::Model);
    let mut bindings = Vec::new();
    for revision in ["first", "second", "third"] {
        spec.origin.source_revision = revision.into();
        let recorded = store
            .publish_set(spec.clone(), store.generation(), &operation())
            .unwrap();
        bindings.push(ArtifactBinding {
            artifact: spec.members[0].artifact.clone(),
            artifact_set: recorded.id,
        });
    }
    let generation = store.generation();
    store
        .reference_bindings("model:fixture", &bindings, generation)
        .unwrap();
    assert_eq!(store.generation(), generation + 1);
    let before = durable(&store);
    store
        .reference_bindings("model:fixture", &bindings, store.generation())
        .unwrap();
    assert_eq!(durable(&store), before);
    let mut invalid = bindings[0].clone();
    invalid.artifact = "f".repeat(64);
    assert!(matches!(
        store.reference_bindings("model:fixture", &[invalid], store.generation()),
        Err(ArtifactError::SetBinding)
    ));
    assert_eq!(durable(&store), before);
    assert!(matches!(
        store.reference_bindings("model:fixture", &bindings[1..], generation),
        Err(ArtifactError::Conflict)
    ));
    assert_eq!(durable(&store), before);
    store
        .reference_bindings("model:fixture", &bindings[1..], store.generation())
        .unwrap();
    assert!(
        store
            .artifact_set(&bindings[0].artifact_set)
            .unwrap()
            .references
            .is_empty()
    );
    for binding in &bindings[1..] {
        assert_eq!(
            store
                .artifact_set(&binding.artifact_set)
                .unwrap()
                .references,
            ["model:fixture"]
        );
        assert!(matches!(
            store.remove_set(&binding.artifact_set, store.generation()),
            Err(ArtifactError::Busy)
        ));
    }
    let inspected = store.inspect().unwrap();
    let primary = inspected
        .artifacts
        .iter()
        .find(|file| file.id == bindings[0].artifact)
        .unwrap();
    assert_eq!(
        primary
            .references
            .iter()
            .filter(|reference| reference.as_str() == "model:fixture")
            .count(),
        1
    );
    store
        .reference_bindings("model:fixture", &[], store.generation())
        .unwrap();
    assert!(
        store
            .inspect()
            .unwrap()
            .artifact_sets
            .iter()
            .all(|set| set.references.is_empty())
    );
    assert_eq!(
        store.inspect().unwrap().charged_bytes,
        inspected.charged_bytes
    );
    assert_no_holds(&store);
}

#[tokio::test]
async fn complete_sets_bind_roles_publish_once_and_keep_all_members_until_lease_release() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let generation = store.generation();
    let published = store
        .publish_set(spec.clone(), generation, &operation())
        .unwrap();
    assert_eq!(store.generation(), generation + 1);
    assert_eq!(published.compatibility, "unqualified");
    assert_eq!(published.specification.members[0].name, "model");
    let recorded = durable(&store);
    let mut reordered = spec.clone();
    reordered.members.reverse();
    let repeated = store
        .publish_set(reordered, store.generation(), &operation())
        .unwrap();
    assert_eq!(repeated.id, published.id);
    assert_eq!(durable(&store), recorded);
    let held = store.hold_set(&published.id, &operation()).unwrap();
    assert_eq!(held.id(), published.id);
    assert_eq!(fs::read(held.primary().path()).unwrap(), MODEL_BYTES);
    assert_eq!(
        fs::read(held.member("projector").unwrap().path()).unwrap(),
        COMPANION_BYTES
    );
    held.verify().unwrap();
    assert_eq!(store.artifact_set(&published.id).unwrap().active_holds, 1);
    let inventory = store.inspect().unwrap();
    assert!(
        inventory
            .artifacts
            .iter()
            .all(|artifact| artifact.active_holds == 1)
    );
    assert!(inventory.artifacts.iter().all(|artifact| {
        artifact
            .references
            .contains(&format!("artifact-set:{}", published.id))
    }));
    assert_eq!(
        store
            .remove_set(&published.id, store.generation())
            .unwrap_err()
            .code(),
        "artifact_busy"
    );
    for member in &spec.members {
        assert_eq!(
            store
                .remove(&member.artifact, store.generation())
                .unwrap_err()
                .code(),
            "artifact_busy"
        );
    }
    drop(held);
    assert_no_holds(&store);
    for member in &spec.members {
        assert_eq!(
            store
                .remove(&member.artifact, store.generation())
                .unwrap_err()
                .code(),
            "artifact_busy"
        );
    }
    let charged = store.inspect().unwrap().charged_bytes;
    store.remove_set(&published.id, store.generation()).unwrap();
    assert_eq!(store.inspect().unwrap().charged_bytes, charged);
    assert!(store.inspect().unwrap().artifact_sets.is_empty());
    for member in &spec.members {
        assert!(store.artifact_path(&member.artifact).is_file());
    }
    let mut role = spec.clone();
    role.members[0].role = ArtifactRole::Tokenizer;
    let changed_role = store
        .publish_set(role, store.generation(), &operation())
        .unwrap();
    assert_ne!(changed_role.id, published.id);
    let mut revision = spec;
    revision.origin.source_revision = "pinned-fixture-v2".into();
    assert_ne!(
        store
            .publish_set(revision, store.generation(), &operation())
            .unwrap()
            .id,
        published.id
    );
}

#[tokio::test]
async fn incomplete_corrupt_and_invalid_sets_preserve_files_manifest_and_reservations() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let original = durable(&store);
    let generation = store.generation();
    let charged = store.inspect().unwrap().charged_bytes;
    let mut missing = spec.clone();
    missing.members[0].artifact = "f".repeat(64);
    assert_eq!(
        store
            .publish_set(missing, generation, &operation())
            .unwrap_err()
            .code(),
        "artifact_not_found"
    );
    let companion = store.artifact_path(&spec.members[0].artifact);
    let mut changed = COMPANION_BYTES.to_vec();
    changed[0] ^= 1;
    fs::write(&companion, &changed).unwrap();
    assert_eq!(
        store
            .publish_set(spec.clone(), generation, &operation())
            .unwrap_err()
            .code(),
        "artifact_hash_mismatch"
    );
    assert_eq!(fs::read(&companion).unwrap(), changed);
    fs::write(&companion, COMPANION_BYTES).unwrap();
    for defect in [
        "duplicate",
        "primary",
        "role",
        "size",
        "origin",
        "count",
        "version",
    ] {
        let mut invalid = spec.clone();
        match defect {
            "duplicate" => invalid.members.push(invalid.members[0].clone()),
            "primary" => invalid.primary = "unknown".into(),
            "role" => invalid.members[0].role = ArtifactRole::Model,
            "size" => invalid.members[0].bytes = 0,
            "origin" => invalid.origin.source_revision = "https://private.invalid/SECRET".into(),
            "count" => invalid.members = vec![invalid.members[0].clone(); 65],
            _ => invalid.schema_version = 2,
        }
        let error = store
            .publish_set(invalid, generation, &operation())
            .unwrap_err();
        assert_eq!(error.code(), "artifact_invalid", "{defect}: {error}");
        assert!(!format!("{error:?} {error}").contains("SECRET"));
    }
    assert_eq!(durable(&store), original);
    assert_eq!(store.generation(), generation);
    assert_eq!(store.inspect().unwrap().charged_bytes, charged);
    assert!(store.inspect().unwrap().artifact_sets.is_empty());
    assert_no_holds(&store);
}

#[tokio::test]
async fn cancellation_deadline_and_stale_publication_never_advertise_a_partial_set() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let generation = store.generation();
    let original = durable(&store);
    let cancelled = Arc::new(AtomicBool::new(true));
    let control = SetOperation::new(Instant::now() + Duration::from_secs(5), cancelled.clone());
    assert_eq!(
        store
            .publish_set(spec.clone(), generation, &control)
            .unwrap_err()
            .code(),
        "artifact_interrupted"
    );
    cancelled.store(false, Ordering::Release);
    assert_eq!(
        store
            .publish_set_checked(spec.clone(), generation, &control, || cancelled
                .store(true, Ordering::Release))
            .unwrap_err()
            .code(),
        "artifact_interrupted"
    );
    assert_eq!(durable(&store), original);
    let expired = SetOperation::new(
        Instant::now() - Duration::from_secs(1),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(
        store
            .publish_set(spec.clone(), generation, &expired)
            .unwrap_err()
            .code(),
        "artifact_deadline"
    );
    assert_eq!(
        store
            .publish_set_checked(spec.clone(), generation, &operation(), || {
                store
                    .reference(
                        &spec.members[1].artifact,
                        "concurrent-reference",
                        true,
                        generation,
                    )
                    .unwrap();
            })
            .unwrap_err()
            .code(),
        "artifact_revision_conflict"
    );
    assert!(store.inspect().unwrap().artifact_sets.is_empty());
    assert_no_holds(&store);
    assert!(
        store
            .runtime
            .lock()
            .unwrap()
            .set_operations
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .all(|control| control.done.load(Ordering::Acquire))
    );
}

#[tokio::test]
async fn failed_publication_retains_members_and_retries_then_restart_checks_complete_bindings() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let generation = store.generation();
    let original = durable(&store);
    let manifest = store.lease.root().join(MANIFEST);
    let backup = store.lease.root().join("manifest-test-backup");
    let error = store
        .publish_set_checked(spec.clone(), generation, &operation(), || {
            fs::rename(&manifest, &backup).unwrap();
            fs::create_dir(&manifest).unwrap();
        })
        .unwrap_err();
    assert!(matches!(error, ArtifactError::Persistence(_)));
    assert_eq!(store.generation(), generation);
    assert!(store.inspect().unwrap().artifact_sets.is_empty());
    assert_no_holds(&store);
    fs::remove_dir(&manifest).unwrap();
    fs::rename(&backup, &manifest).unwrap();
    assert_eq!(durable(&store), original);
    let published = store.publish_set(spec, generation, &operation()).unwrap();
    let lease = store.lease.clone();
    let scope = store.scope.clone();
    let limits = store.limits.clone();
    let policy = store.policy;
    drop(store);
    let restored =
        ArtifactStore::open_scoped(lease.clone(), limits.clone(), policy, scope.clone()).unwrap();
    let held = restored.hold_set(&published.id, &operation()).unwrap();
    assert_eq!(held.specification(), &*published.specification);
    let path = held.member("projector").unwrap().path().to_owned();
    drop(held);
    fs::remove_file(path).unwrap();
    assert!(restored.hold_set(&published.id, &operation()).is_err());
    assert_eq!(
        restored.artifact_set(&published.id).unwrap().active_holds,
        0
    );
    let mut broken: Value = serde_json::from_slice(&durable(&restored)).unwrap();
    broken["artifact_sets"][&published.id]["specification"]["members"][1]["role"] =
        json!("tokenizer");
    let broken = serde_json::to_vec(&broken).unwrap();
    fs::write(&manifest, &broken).unwrap();
    drop(restored);
    assert!(ArtifactStore::open_scoped(lease, limits, policy, scope).is_err());
    assert_eq!(fs::read(manifest).unwrap(), broken);
}

#[tokio::test]
async fn legacy_reads_are_in_memory_only_and_missing_null_or_duplicate_set_indexes_are_rejected() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let published = store
        .publish_set(spec, store.generation(), &operation())
        .unwrap();
    let canonical = durable(&store);
    let lease = store.lease.clone();
    let scope = store.scope.clone();
    let limits = store.limits.clone();
    let policy = store.policy;
    let manifest = lease.root().join(MANIFEST);
    drop(store);
    let mut legacy: Value = serde_json::from_slice(&canonical).unwrap();
    legacy["schema_version"] = json!(1);
    legacy.as_object_mut().unwrap().remove("artifact_sets");
    let original = serde_json::to_vec(&legacy).unwrap();
    fs::write(&manifest, &original).unwrap();
    let migrated =
        ArtifactStore::open_scoped(lease.clone(), limits.clone(), policy, scope.clone()).unwrap();
    assert!(migrated.inspect().unwrap().artifact_sets.is_empty());
    assert_eq!(fs::read(&manifest).unwrap(), original);
    drop(migrated);
    let mut cases = Vec::new();
    let mut absent: Value = serde_json::from_slice(&canonical).unwrap();
    absent.as_object_mut().unwrap().remove("artifact_sets");
    cases.push(serde_json::to_vec(&absent).unwrap());
    let mut null = absent.clone();
    null["artifact_sets"] = Value::Null;
    cases.push(serde_json::to_vec(&null).unwrap());
    let mut legacy_null = legacy;
    legacy_null["artifact_sets"] = Value::Null;
    cases.push(serde_json::to_vec(&legacy_null).unwrap());
    let mut duplicate: Value = serde_json::from_slice(&canonical).unwrap();
    let records = duplicate
        .as_object_mut()
        .unwrap()
        .remove("artifact_sets")
        .unwrap();
    let record = serde_json::to_string(&records[&published.id]).unwrap();
    let prefix = serde_json::to_string(&duplicate).unwrap();
    cases.push(
        format!(
            "{},\"artifact_sets\":{{\"{}\":{},\"{}\":{}}}}}",
            &prefix[..prefix.len() - 1],
            published.id,
            record,
            published.id,
            record
        )
        .into_bytes(),
    );
    for bytes in cases {
        fs::write(&manifest, &bytes).unwrap();
        assert!(
            ArtifactStore::open_scoped(lease.clone(), limits.clone(), policy, scope.clone())
                .is_err()
        );
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
    }
}

#[tokio::test]
async fn repeated_member_content_has_one_physical_lease_and_set_capacity_is_bounded() {
    let (_directory, store) = store();
    let mut spec = specification(&store).await;
    spec.members[0].artifact = spec.members[1].artifact.clone();
    spec.members[0].bytes = spec.members[1].bytes;
    let published = store
        .publish_set(spec.clone(), store.generation(), &operation())
        .unwrap();
    let held = store.hold_set(&published.id, &operation()).unwrap();
    assert_eq!(
        held.primary().path(),
        held.member("projector").unwrap().path()
    );
    assert_eq!(
        store
            .inspect()
            .unwrap()
            .artifacts
            .iter()
            .map(|artifact| artifact.active_holds)
            .sum::<usize>(),
        1
    );
    let runtime = store.runtime.lock().unwrap();
    let (released, observed) = std::sync::mpsc::channel();
    let release = std::thread::spawn(move || {
        drop(held);
        released.send(()).unwrap();
    });
    let completed = observed.recv_timeout(Duration::from_secs(1));
    drop(runtime);
    release.join().unwrap();
    completed.expect("whole-set release waited for manifest ownership");
    assert_no_holds(&store);
    let directory = tempfile::tempdir().unwrap();
    let limited = ArtifactStore::open(
        DirectoryLease::acquire(directory.path().join("store")).unwrap(),
        ArtifactLimits {
            maximum_sets: 1,
            ..ArtifactLimits::default()
        },
        SourcePolicy::Offline,
    )
    .unwrap();
    let spec = specification(&limited).await;
    limited
        .publish_set(spec.clone(), limited.generation(), &operation())
        .unwrap();
    let original = durable(&limited);
    let mut other = spec;
    other.origin.source_revision.push_str("-other");
    assert_eq!(
        limited
            .publish_set(other, limited.generation(), &operation())
            .unwrap_err()
            .code(),
        "artifact_limit"
    );
    assert_eq!(durable(&limited), original);
    assert_no_holds(&limited);
}

#[tokio::test]
async fn shutdown_waits_for_the_actual_set_preparation_before_acknowledging_completion() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let original = durable(&store);
    let writer = store.clone();
    let generation = store.generation();
    let (ready, observed) = std::sync::mpsc::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    let job = std::thread::spawn(move || {
        writer.publish_set_checked(spec, generation, &operation(), || {
            ready.send(()).unwrap();
            waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        })
    });
    tokio::task::spawn_blocking(move || observed.recv_timeout(Duration::from_secs(3)))
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .inspect()
            .unwrap()
            .artifacts
            .iter()
            .all(|artifact| artifact.active_holds == 1)
    );
    let stopping = store.clone();
    let shutdown = tokio::spawn(async move {
        stopping.shutdown().await;
    });
    for _ in 0..100 {
        if store.stopping.load(Ordering::Acquire) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(store.stopping.load(Ordering::Acquire));
    assert!(!shutdown.is_finished());
    release.send(()).unwrap();
    let result = tokio::task::spawn_blocking(move || job.join().unwrap())
        .await
        .unwrap();
    assert_eq!(result.unwrap_err().code(), "artifact_interrupted");
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(durable(&store), original);
    assert_no_holds(&store);
    assert!(store.inspect().unwrap().artifact_sets.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn moved_roots_before_publication_preserve_both_owned_members_and_foreign_replacement() {
    let (_directory, store) = store();
    let spec = specification(&store).await;
    let original = durable(&store);
    let root = store.lease.root().to_owned();
    let retained = root.with_file_name("retained-original");
    let replacement = root.with_file_name("foreign-replacement");
    let result = store.publish_set_checked(spec, store.generation(), &operation(), || {
        fs::rename(&root, &retained).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("foreign-content"), b"preserve foreign bytes").unwrap();
    });
    assert_eq!(result.unwrap_err().code(), "storage_root_unavailable");
    assert_eq!(fs::read(retained.join(MANIFEST)).unwrap(), original);
    assert_eq!(
        fs::read(root.join("foreign-content")).unwrap(),
        b"preserve foreign bytes"
    );
    assert!(
        store
            .runtime
            .lock()
            .unwrap()
            .manifest
            .artifact_sets
            .is_empty()
    );
    assert!(
        store
            .runtime
            .lock()
            .unwrap()
            .manifest
            .artifacts
            .values()
            .all(|artifact| artifact.active_holds() == 0)
    );
    fs::rename(&root, &replacement).unwrap();
    fs::rename(&retained, &root).unwrap();
    assert_eq!(durable(&store), original);
    assert_no_holds(&store);
}

#[tokio::test]
async fn executable_primary_bindings_share_whole_set_ownership_and_publish_both_references_atomically()
 {
    let (_directory, store) = store();
    let mut spec = specification(&store).await;
    let companion = store
        .publish_set(spec.clone(), store.generation(), &operation())
        .unwrap();
    let primary = spec
        .members
        .iter()
        .find(|member| member.role == ArtifactRole::Model)
        .unwrap()
        .artifact
        .clone();
    assert_eq!(
        store
            .hold_binding(&primary, Some(&companion.id))
            .err()
            .unwrap()
            .code(),
        "artifact_set_execution_unsupported"
    );
    spec.members
        .retain(|member| member.role == ArtifactRole::Model);
    let published = store
        .publish_set(spec, store.generation(), &operation())
        .unwrap();
    assert_eq!(
        store
            .hold_binding(&"f".repeat(64), Some(&published.id))
            .err()
            .unwrap()
            .code(),
        "artifact_set_binding_mismatch"
    );
    let binding = store.hold_binding(&primary, Some(&published.id)).unwrap();
    assert_eq!(binding.set_id(), Some(published.id.as_str()));
    binding.verify_set().unwrap();
    assert_eq!(store.artifact_set(&published.id).unwrap().active_holds, 1);
    assert_eq!(
        store
            .inspect()
            .unwrap()
            .artifacts
            .iter()
            .find(|artifact| artifact.id == primary)
            .unwrap()
            .active_holds,
        1
    );
    let generation = store.generation();
    store
        .reference_binding(
            &primary,
            Some(&published.id),
            "model:bound-primary",
            true,
            generation,
        )
        .unwrap();
    assert_eq!(store.generation(), generation + 1);
    assert_eq!(
        store.artifact_set(&published.id).unwrap().references,
        vec!["model:bound-primary"]
    );
    let recorded = durable(&store);
    store
        .reference_binding(
            &primary,
            Some(&published.id),
            "model:bound-primary",
            true,
            store.generation(),
        )
        .unwrap();
    assert_eq!(durable(&store), recorded);
    drop(binding);
    assert_no_holds(&store);
    assert_eq!(
        store
            .remove_set(&published.id, store.generation())
            .unwrap_err()
            .code(),
        "artifact_busy"
    );
    store
        .reference_binding(
            &primary,
            Some(&published.id),
            "model:bound-primary",
            false,
            store.generation(),
        )
        .unwrap();
    assert!(
        store
            .artifact_set(&published.id)
            .unwrap()
            .references
            .is_empty()
    );
    assert!(
        !store
            .inspect()
            .unwrap()
            .artifacts
            .iter()
            .find(|artifact| artifact.id == primary)
            .unwrap()
            .references
            .contains(&"model:bound-primary".into())
    );
    store.remove_set(&published.id, store.generation()).unwrap();
}
