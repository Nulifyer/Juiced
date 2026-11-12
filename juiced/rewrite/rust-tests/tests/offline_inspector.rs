mod support;

use juiced_service::{
    cache::{CacheStore, SnapshotIdentity},
    storage::DirectoryLease,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::Arc,
    time::Duration,
};
use support::binary;

fn disk(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            assert!(!entry.file_type().unwrap().is_symlink());
            if path.is_dir() {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf(), None);
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    Some(std::fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}
async fn inspect(root: &Path, cwd: &Path) -> Output {
    let child = tokio::process::Command::new(binary("lemond", "JUICED_TEST_SERVER"))
        .arg("--inspect-snapshot-store")
        .arg(root)
        .current_dir(cwd)
        .env_remove("LEMONADE_API_KEY")
        .env_remove("LEMONADE_ADMIN_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .expect("offline inspector deadline")
        .unwrap()
}
fn report(output: Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "inspector stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).expect("one JSON report on stdout");
    for field in [
        "read_only",
        "non_atomic",
        "no_recovery_authority",
        "cooperative_deadline",
    ] {
        assert_eq!(report[field], true, "{field}");
    }
    for field in [
        "recovery_authorized",
        "application_authorized",
        "writer_termination_verified",
    ] {
        assert_eq!(report[field], false, "{field}");
    }
    let serialized = report.to_string();
    for private in ["offline-fixture", "offline-epoch"] {
        assert!(!serialized.contains(private));
    }
    assert!(!serialized.contains(&"b".repeat(64)));
    report
}
fn contains_code(value: &Value, code: &str) -> bool {
    match value {
        Value::String(value) => value == code,
        Value::Array(values) => values.iter().any(|value| contains_code(value, code)),
        Value::Object(values) => values.values().any(|value| contains_code(value, code)),
        _ => false,
    }
}
fn identity() -> SnapshotIdentity {
    SnapshotIdentity {
        execution_fingerprint: "a".repeat(64),
        principal: "offline-fixture".into(),
        credential_epoch: "offline-epoch".into(),
        protocol: "openai.completions".into(),
        session_digest: "b".repeat(64),
    }
}
fn store(root: &Path) -> Arc<CacheStore> {
    CacheStore::open(DirectoryLease::acquire(root).unwrap()).unwrap()
}

#[tokio::test]
async fn offline_inspector_does_not_create_missing_roots_or_treat_native_legacy_as_rust_state() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing cache");
    let value = report(inspect(&missing, cwd.path()).await, 2);
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["format_recognized"], false);
    assert!(contains_code(&value, "root_unavailable_or_invalid"));
    assert!(!missing.exists());
    assert!(disk(cwd.path()).is_empty());
    assert!(disk(directory.path()).is_empty());
    let empty = directory.path().join("empty cache");
    std::fs::create_dir(&empty).unwrap();
    let value = report(inspect(&empty, cwd.path()).await, 2);
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["format_recognized"], false);
    assert!(contains_code(&value, "manifest_unavailable_or_invalid"));
    assert!(disk(&empty).is_empty());
    assert!(disk(cwd.path()).is_empty());
    let legacy = directory.path().join("legacy cache");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(legacy.join("snapshots.json"),br#"{"version":1,"slots":[{"file":"native-slot.bin"}],"private":"native-legacy-fixture-content"}"#).unwrap();
    std::fs::write(
        legacy.join("native-slot.bin"),
        b"retained legacy fixture bytes",
    )
    .unwrap();
    let before = disk(&legacy);
    let value = report(inspect(&legacy, cwd.path()).await, 2);
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["format_recognized"], false);
    assert!(contains_code(&value, "manifest_invalid"));
    assert!(!value.to_string().contains("native-legacy-fixture-content"));
    assert_eq!(disk(&legacy), before);
    assert!(disk(cwd.path()).is_empty());
}

#[tokio::test]
async fn offline_inspector_reads_current_saved_store_while_writer_lease_is_live_without_modifying_files()
 {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("current cache");
    let cwd = tempfile::tempdir().unwrap();
    let cache = store(&root);
    let pending = cache.begin(identity(), 0, 1024).unwrap();
    std::fs::write(pending.path(), b"committed Rust snapshot fixture bytes").unwrap();
    let saved = cache.publish(&pending, pending.revision()).unwrap();
    drop(pending);
    let before = disk(&root);
    let value = report(inspect(&root, cwd.path()).await, 0);
    assert_eq!(value["format"], "juiced-rust-cache-v1");
    assert_eq!(value["format_recognized"], true);
    assert_eq!(value["validation_complete"], true);
    assert_eq!(value["status"], "complete");
    for field in ["root_verified", "manifest_verified", "inventory_complete"] {
        assert_eq!(value[field], true, "{field}");
    }
    assert_eq!(value["records_checked"], 1);
    assert_eq!(value["files_checked"], 3);
    assert!(value["files_enumerated"].as_u64().unwrap() >= 2);
    assert!(value["manifest_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        value["hash_bytes"],
        b"committed Rust snapshot fixture bytes".len()
    );
    assert_eq!(
        value["charged_bytes"],
        b"committed Rust snapshot fixture bytes".len()
    );
    assert_eq!(value["snapshots"][0]["id"], saved.id);
    assert_eq!(value["snapshots"][0]["state"], "saved");
    assert_eq!(value["snapshots"][0]["checksum_valid"], true);
    assert!(value["issues"].as_array().unwrap().is_empty());
    assert!(!value.to_string().contains(root.to_str().unwrap()));
    assert_eq!(disk(&root), before);
    assert!(disk(cwd.path()).is_empty());
    assert_eq!(
        cache.list("offline-fixture", "offline-epoch").unwrap()[0].id,
        saved.id
    );
    drop(cache);
    let closed = disk(&root);
    let value = report(inspect(&root, cwd.path()).await, 0);
    assert_eq!(value["validation_complete"], true);
    assert_eq!(disk(&root), closed);
    std::fs::remove_file(root.join(".juiced-store.lock")).unwrap();
    let without_control = disk(&root);
    let value = report(inspect(&root, cwd.path()).await, 0);
    assert_eq!(value["format_recognized"], true);
    assert_eq!(value["status"], "complete");
    assert_eq!(value["files_checked"], 2);
    assert!(!root.join(".juiced-store.lock").exists());
    assert_eq!(disk(&root), without_control);
}

#[tokio::test]
async fn offline_inspector_preserves_corrupt_and_unconfirmed_snapshot_bytes_and_charge() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("uncertain cache");
    let cwd = tempfile::tempdir().unwrap();
    let cache = store(&root);
    let pending = cache.begin(identity(), 0, 1024).unwrap();
    std::fs::write(pending.path(), b"unconfirmed snapshot writer bytes").unwrap();
    drop(pending);
    let before = disk(&root);
    let value = report(inspect(&root, cwd.path()).await, 2);
    assert_eq!(value["status"], "partial");
    assert_eq!(value["format_recognized"], true);
    assert_eq!(value["validation_complete"], false);
    assert!(contains_code(&value, "unresolved_snapshot_operation"));
    assert_eq!(value["charged_bytes"], 1024);
    assert!(
        value["issues"]
            .as_array()
            .is_some_and(|issues| !issues.is_empty())
    );
    assert_eq!(disk(&root), before);
    assert_eq!(cache.inspect().unwrap().charged_bytes, 1024);
    let clean_root = directory.path().join("corrupt cache");
    let saved_store = store(&clean_root);
    let pending = saved_store.begin(identity(), 0, 1024).unwrap();
    std::fs::write(pending.path(), b"verified fixture bytes").unwrap();
    saved_store.publish(&pending, pending.revision()).unwrap();
    let path = pending.path().to_owned();
    drop(pending);
    std::fs::write(path, b"corrupted fixture bytes").unwrap();
    let before = disk(&clean_root);
    let value = report(inspect(&clean_root, cwd.path()).await, 2);
    assert_eq!(value["status"], "partial");
    assert_eq!(value["format_recognized"], true);
    assert!(contains_code(&value, "snapshot_checksum_mismatch"));
    assert_eq!(value["snapshots"][0]["checksum_valid"], false);
    assert!(
        value["issues"]
            .as_array()
            .is_some_and(|issues| !issues.is_empty())
    );
    assert_eq!(disk(&clean_root), before);
    assert!(disk(cwd.path()).is_empty());
}
