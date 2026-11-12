mod support;

use juiced_service::storage::DirectoryLease;
use serde_json::json;
use support::{Server, filesystem::tree, rejected_manager};

const GUIDANCE: &str = "Preserve the file and restore a valid supported configuration.";

fn coordinate(output: &str, field: &str) -> usize {
    let prefix = format!("{field}: Some(");
    let value = output
        .split_once(&prefix)
        .expect("safe parse coordinate is missing")
        .1
        .split_once(')')
        .expect("safe parse coordinate is unterminated")
        .0
        .parse::<usize>()
        .expect("safe parse coordinate is not numeric");
    assert!(value > 0, "parse coordinates must be positive");
    value
}

async fn reject(server: &Server, source: &[u8], marker: &str, decode: bool) {
    std::fs::write(&server.config_path, source).unwrap();
    drop(DirectoryLease::acquire(server.directory.path()).unwrap());
    let before = tree(server.directory.path());
    let output = rejected_manager(&server.config_path).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "invalid startup wrote stdout");
    for bytes in [&output.stdout, &output.stderr] {
        assert!(
            !bytes
                .windows(marker.len())
                .any(|part| part == marker.as_bytes()),
            "startup diagnostics exposed a private input marker"
        );
        assert!(
            !bytes.windows(source.len()).any(|part| part == source),
            "startup diagnostics exposed raw configuration input"
        );
    }
    let stderr = String::from_utf8(output.stderr).expect("safe startup diagnostic must be UTF-8");
    assert!(
        stderr.len() < 1024,
        "safe startup diagnostic exceeded its bounded metadata scope"
    );
    assert!(stderr.contains("configuration_recovery_required"));
    assert!(stderr.contains(GUIDANCE));
    if decode {
        assert!(stderr.contains("stage: \"decode\""));
        assert!(stderr.contains("category: Some(\"data\")"));
        assert!(
            coordinate(&stderr, "line") <= source.iter().filter(|byte| **byte == b'\n').count() + 1
        );
        assert!(coordinate(&stderr, "column") <= source.len());
    } else {
        assert!(stderr.contains("stage: \"validate\""));
        for field in ["category", "line", "column"] {
            assert!(stderr.contains(&format!("{field}: None")));
        }
    }
    assert_eq!(tree(server.directory.path()), before);
    for model in ["fixture.one", "fixture.two", "fixture.disabled"] {
        assert!(!server.event(model, "worker-startup", "started"));
    }
    for store in ["kv-cache", "artifacts"] {
        assert!(
            !server.directory.path().join(store).exists(),
            "invalid configuration opened a managed store"
        );
    }
}

#[tokio::test]
async fn top_level_private_json_string_rejects_daemon_cli_startup_with_safe_decode_locations() {
    let server = Server::prepare(false, |_| {});
    let marker = "PRIVATE_TOP_LEVEL_CONFIG_STRING_9d6ed510";
    let source = serde_json::to_vec(&json!(marker)).unwrap();
    reject(&server, &source, marker, true).await;
}

#[tokio::test]
async fn private_unknown_policy_enum_rejects_daemon_cli_startup_without_printing_the_enum_value() {
    let mut server = Server::prepare(false, |_| {});
    let marker = "PRIVATE_ARTIFACT_POLICY_694c8558";
    server.config["artifact_source_policy"] = json!(marker);
    for model in server.config["models"].as_array_mut().unwrap() {
        model["policy"] = json!("hot");
    }
    let source = serde_json::to_vec_pretty(&server.config).unwrap();
    reject(&server, &source, marker, true).await;
}

#[tokio::test]
async fn private_model_id_in_semantic_failure_rejects_daemon_cli_startup_without_identity_or_source_details()
 {
    let mut server = Server::prepare(false, |_| {});
    let marker = "PRIVATE_INVALID_MODEL_ID_32e42e26";
    server.config["models"][0]["id"] = json!(marker);
    server.config["models"][0]["resident_bytes"] = json!(0);
    for model in server.config["models"].as_array_mut().unwrap() {
        model["policy"] = json!("hot");
    }
    let source = serde_json::to_vec_pretty(&server.config).unwrap();
    reject(&server, &source, marker, false).await;
}
