#![cfg(unix)]

mod support;

use juiced_service::native_store::descriptor::STORE_ROOT;
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsStr, path::Path, process::Output, time::Duration};
use support::Server;

const PRIVATE: &str = "settings-backup-private-fixture";

async fn command(configuration: &Path, arguments: &[&OsStr]) -> Output {
    let output = support::cli::offline(configuration, arguments, Duration::from_secs(10)).await;
    if !output.status.success() {
        eprintln!(
            "Offline action {:?} exited {:?}",
            arguments.first(),
            output.status.code()
        );
    }
    for bytes in [&output.stdout, &output.stderr] {
        assert!(
            !bytes
                .windows(PRIVATE.len())
                .any(|bytes| bytes == PRIVATE.as_bytes())
        );
    }
    output
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
async fn imported() -> Server {
    let server = Server::prepare(true, |config| config["models"] = json!([]));
    let native = tempfile::tempdir().unwrap();
    for name in [
        "config.json",
        "jobs.json",
        "execution_profiles.json",
        "user_models.json",
        "native_helpers.json",
    ] {
        std::fs::write(native.path().join(name), b"{}\n").unwrap();
    }
    std::fs::write(
        native.path().join("recipe_options.json"),
        format!("{{\"opaque\":{{\"private\":\"{PRIVATE}\",\"literal\":1e+004}}}}\n"),
    )
    .unwrap();
    success(
        command(
            &server.config_path,
            &[
                OsStr::new("--import-native-settings-copy"),
                native.path().as_os_str(),
            ],
        )
        .await,
    );
    server
}
fn image(configuration: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut image = BTreeMap::from([(
        "configuration".to_owned(),
        std::fs::read(configuration).unwrap(),
    )]);
    for entry in std::fs::read_dir(configuration.parent().unwrap().join(STORE_ROOT)).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        image.insert(
            entry.file_name().into_string().unwrap(),
            std::fs::read(entry.path()).unwrap(),
        );
    }
    image
}

fn tree(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink());
            if kind.is_dir() {
                walk(root, &entry.path(), files);
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

#[tokio::test]
async fn actual_offline_backup_restore_restart_and_live_owner_refusal_preserve_exact_settings() {
    let mut source = imported().await;
    let before = image(&source.config_path);
    let directory = tempfile::tempdir().unwrap();
    let capsule = directory.path().join("capsule");
    let exported = success(
        command(
            &source.config_path,
            &[OsStr::new("--backup-settings-copy"), capsule.as_os_str()],
        )
        .await,
    );
    assert_eq!(exported["complete"], true);
    assert_eq!(image(&source.config_path), before);
    source.start().await;
    let refused = directory.path().join("live-owner-refused");
    let output = command(
        &source.config_path,
        &[OsStr::new("--backup-settings-copy"), refused.as_os_str()],
    )
    .await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!refused.exists());
    assert_eq!(source.get("health").await["status"], "ok");
    assert_eq!(source.get("juiced/catalog").await["data"], json!([]));
    source.stop();
    assert_eq!(image(&source.config_path), before);
    let target = directory.path().join("restored/config.json");
    let restored = success(
        command(
            &target,
            &[OsStr::new("--restore-settings-copy"), capsule.as_os_str()],
        )
        .await,
    );
    assert_eq!(restored["complete"], true);
    assert_eq!(restored["manifest_sha256"], exported["manifest_sha256"]);
    assert_eq!(image(&target), before);
    let retry = success(
        command(
            &target,
            &[OsStr::new("--restore-settings-copy"), capsule.as_os_str()],
        )
        .await,
    );
    assert_eq!(retry["manifest_sha256"], exported["manifest_sha256"]);
    let mut server = Server::prepare(true, |config| config["models"] = json!([]));
    server.config_path = target.clone();
    server.start().await;
    assert_eq!(server.get("health").await["status"], "ok");
    assert_eq!(server.get("juiced/catalog").await["data"], json!([]));
    server.stop();
    assert_eq!(image(&target), before);
    let used = tree(target.parent().unwrap());
    let refused = command(
        &target,
        &[OsStr::new("--restore-settings-copy"), capsule.as_os_str()],
    )
    .await;
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("destination_occupied"));
    assert_eq!(tree(target.parent().unwrap()), used);
    assert_eq!(image(&target), before);
    assert_eq!(image(&source.config_path), before);
}

#[tokio::test]
async fn corrupt_capsule_and_mixed_serving_options_refuse_without_creating_configuration() {
    let source = imported().await;
    let before = image(&source.config_path);
    let directory = tempfile::tempdir().unwrap();
    let capsule = directory.path().join("capsule");
    success(
        command(
            &source.config_path,
            &[OsStr::new("--backup-settings-copy"), capsule.as_os_str()],
        )
        .await,
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(capsule.join("backup.json")).unwrap()).unwrap();
    let name = manifest["settings"][0]["name"].as_str().unwrap();
    let file = capsule.join(name);
    let damaged = vec![b'!'; std::fs::metadata(&file).unwrap().len() as usize];
    std::fs::write(&file, &damaged).unwrap();
    let target = directory.path().join("refused/config.json");
    let output = command(
        &target,
        &[OsStr::new("--restore-settings-copy"), capsule.as_os_str()],
    )
    .await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("settings_backup_recovery_required"));
    assert!(!target.parent().unwrap().exists());
    assert_eq!(std::fs::read(file).unwrap(), damaged);
    let unused = directory.path().join("invalid-mode");
    for flags in [
        vec![
            OsStr::new("--backup-settings-copy"),
            unused.as_os_str(),
            OsStr::new("--listen"),
            OsStr::new("127.0.0.1:0"),
        ],
        vec![
            OsStr::new("--restore-settings-copy"),
            capsule.as_os_str(),
            OsStr::new("--backup-settings-copy"),
            unused.as_os_str(),
        ],
    ] {
        let output = command(&source.config_path, &flags).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!unused.exists());
    }
    assert_eq!(image(&source.config_path), before);
}
