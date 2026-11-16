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

async fn settled_follow_up(server: &Server, pull: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let view = server.get(&format!("juiced/library/pulls/{pull}")).await;
        if view["then"]["state"] != "pending" {
            return view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "follow-up did not settle: {view}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn update_downloads_new_upstream_files_and_moves_the_model_with_its_settings() {
    let hub = Hub::start().await;
    let file = "Process-Q4_K_M.gguf";
    let mut server = Server::prepare(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.start().await;
    let (status, pulled) = server
        .post(
            "juiced/library/pull",
            json!({"repo": REPO, "file": file, "expected_generation": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{pulled}");
    let old = hub.file(REPO, file).sha256();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while server
        .get(&format!(
            "juiced/library/pulls/{}",
            pulled["pull"].as_str().unwrap()
        ))
        .await["state"]
        != "complete"
    {
        assert!(
            std::time::Instant::now() < deadline,
            "first pull did not finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let template = &server.config["models"][0];
    let registration = json!({"id":"updating","artifact":old,"model_file":template["model_file"],
        "provider":"llama-cpp","command":template["command"],
        "args":["--port","{port}","--model","{model}","--ctx-size","256","--parallel","1","--n-gpu-layers","0"],
        "resident_bytes":16,"max_inflight":1,"operations":["completions"],"policy":"disabled","revision":0});
    let (status, registered) = server.post("models/register", registration).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let revision = server.model("updating").await["revision"].as_u64().unwrap();

    let (status, refused) = server
        .post(
            "models/fixture.one/update",
            json!({"expected_revision": server.model("fixture.one").await["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "update_unsupported");
    let (status, stale) = server
        .post(
            "models/updating/update",
            json!({"expected_revision": revision + 1}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "revision_conflict");
    for key in [None, server.read_key()] {
        let (status, _) = server
            .json(
                Method::POST,
                "/v1/models/updating/update",
                Some(json!({"expected_revision": revision})),
                key,
            )
            .await;
        assert!(
            matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
            "{status}"
        );
    }

    let fresh: Vec<u8> = (0..24_500).map(|index| (index % 233) as u8 ^ 9).collect();
    let new = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&fresh));
    hub.replace(REPO, file, fresh);
    let (status, checked) = server.post("models/check-updates", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["models"], json!(["updating"]));
    let (status, started) = server
        .post(
            "models/updating/update",
            json!({"expected_revision": revision}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["then"]["kind"], "update");
    assert_eq!(started["then"]["model"], "updating");
    let settled = settled_follow_up(&server, started["pull"].as_str().unwrap()).await;
    assert_eq!(settled["state"], "complete", "{settled}");
    assert_eq!(settled["then"]["state"], "done", "{settled}");
    let model = server.model("updating").await;
    assert_eq!(model["policy"], "disabled");
    assert!(model["revision"].as_u64().unwrap() > revision);
    let (status, rechecked) = server.post("models/check-updates", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{rechecked}");
    assert_eq!(rechecked["models"], json!([]), "{rechecked}");
    let artifacts = server.get("juiced/artifacts?limit=64").await;
    let references = |digest: &str| {
        artifacts["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|artifact| artifact["id"] == digest)
            .unwrap_or_else(|| panic!("{digest} missing from {artifacts}"))["references"]
            .clone()
    };
    assert_eq!(references(&new), json!(["model:updating"]));
    assert_eq!(references(&old), json!([]));
}
