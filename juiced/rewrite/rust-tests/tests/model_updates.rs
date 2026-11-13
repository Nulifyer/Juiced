mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use support::{
    PREFIXES, Server,
    huggingface::{Hub, REVISION},
};

const REPO: &str = "fixture/Process-GGUF";

/// A Hugging Face cache snapshot whose file links to `blobs/<digest>`, as native left them.
fn cached(root: &Path, file: &str, digest: &str) -> PathBuf {
    let repo = root.join("hub/models--fixture--Process-GGUF");
    let blob = repo.join("blobs").join(digest);
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, b"cached weights").unwrap();
    let snapshot = repo.join("snapshots").join(REVISION);
    std::fs::create_dir_all(&snapshot).unwrap();
    let path = snapshot.join(file);
    std::os::unix::fs::symlink(format!("../../blobs/{digest}"), &path).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test]
async fn check_updates_compares_cached_digests_with_the_hub_and_marks_the_model_list() {
    let hub = Hub::start().await;
    let cache = tempfile::tempdir().unwrap();
    let current = cached(
        cache.path(),
        "Process-Q4_K_M.gguf",
        &hub.file(REPO, "Process-Q4_K_M.gguf").sha256(),
    );
    let stale = cached(cache.path(), "Process-Q8_0.gguf", &"0".repeat(64));
    let gone = cached(cache.path(), "Process-Removed.gguf", &"1".repeat(64));
    let mut server = Server::prepare(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
        let template = config["models"][0].clone();
        let mut models = vec![config["models"][1].clone()];
        for (id, file) in [("current", &current), ("stale", &stale), ("gone", &gone)] {
            let mut model = template.clone();
            model["id"] = json!(id);
            model["model_file"] = json!(file);
            model["args"][5] = json!(format!("{id}.events"));
            models.push(model);
        }
        config["models"] = Value::Array(models);
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.start().await;
    let listed = |models: &Value, id: &str| {
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == id)
            .unwrap()["update_available"]
            .clone()
    };
    let before = server.get("models").await;
    assert_eq!(listed(&before, "stale"), false);
    for key in [None, server.read_key()] {
        let (status, _) = server
            .json(Method::POST, "/v1/models/check-updates", None, key)
            .await;
        assert!(
            matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
            "{status}"
        );
    }
    for prefix in PREFIXES {
        let (status, reply) = server
            .json(
                Method::POST,
                &format!("{prefix}/models/check-updates"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(reply["status"], "failed", "{reply}");
        assert_eq!(reply["updates_available"], 1);
        assert_eq!(reply["models"], json!(["stale"]));
        assert_eq!(reply["failed_models"], json!({"gone": "file_not_upstream"}));
        assert_eq!(reply["checked"], 3);
    }
    let after = server.get("models").await;
    assert_eq!(listed(&after, "stale"), true);
    assert_eq!(listed(&after, "current"), false);
    assert_eq!(listed(&after, "fixture.two"), false);
}

#[tokio::test]
async fn check_updates_refuses_while_the_hub_is_offline() {
    let server = Server::configured(true, |config| {
        config["artifact_source_policy"] = json!("offline");
    })
    .await;
    let (status, reply) = server
        .json(
            Method::POST,
            "/v1/models/check-updates",
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    assert_eq!(reply["error"]["code"], "update_check_offline");
}
