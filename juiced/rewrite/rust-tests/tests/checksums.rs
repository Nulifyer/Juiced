#![cfg(target_os = "linux")]
mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use support::{ADMIN_KEY, Server, metadata_gguf};

const BYTES: usize = 16 * 1024 * 1024;
/// Everything a warmed-up service reads for one load or one start, with room to spare.
const QUIET: u64 = 4 * 1024 * 1024;

fn weights(fill: u8) -> Vec<u8> {
    let mut bytes = metadata_gguf(2048);
    bytes.resize(BYTES, fill);
    bytes
}

/// Bytes the service process has read from files since it started.
fn read_bytes(server: &Server) -> u64 {
    std::fs::read_to_string(format!("/proc/{}/io", server.pid()))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .parse()
        .unwrap()
}

async fn load(server: &Server, model: &str) -> (StatusCode, Value, u64) {
    let before = read_bytes(server);
    let (status, reply) = server.post("load", json!({"model":model})).await;
    (status, reply, read_bytes(server) - before)
}

async fn unload(server: &Server, model: &str) {
    let (status, reply) = server.post("unload", json!({"model":model})).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

/// The first launch reads the worker executable; later launches reuse its digest.
async fn warm(server: &Server) {
    let (status, reply, _) = load(server, "fixture.one").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    unload(server, "fixture.one").await;
}

async fn fingerprint(server: &Server, model: &str) -> String {
    server.get(&format!("juiced/context?model={model}")).await["applied"]["execution_fingerprint"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn remembered(server: &Server, path: &Path) -> Value {
    let record: Value = serde_json::from_slice(
        &std::fs::read(server.directory.path().join("file-checksums.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["schema_version"], 1);
    record["files"][path.to_str().unwrap()].clone()
}

#[tokio::test]
async fn a_stored_model_is_read_once_until_it_changes_or_remembering_is_turned_off() {
    let mut server = Server::configured(true, |config| {
        config["upload_max_bytes"] = json!(BYTES);
    })
    .await;
    warm(&server).await;
    let original = weights(0x5a);
    let artifact = format!("{:x}", Sha256::digest(&original));
    let uploaded: Value = server
        .client
        .post(format!(
            "{}/v1/juiced/library/upload?filename=stored.gguf&size={BYTES}",
            server.base
        ))
        .bearer_auth(ADMIN_KEY)
        .header("content-type", "application/octet-stream")
        .body(original.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(uploaded["artifact"], artifact, "{uploaded}");

    let mut registration = server.config["models"][0].clone();
    registration["id"] = json!("stored");
    registration["model_file"] = json!("");
    registration["artifact"] = json!(artifact);
    registration["policy"] = json!("disabled");
    registration["revision"] = json!(0);
    registration["args"][5] = json!(server.directory.path().join("stored.events"));
    let before = read_bytes(&server);
    let (status, registered) = server.post("models/register", registration).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let read = read_bytes(&server) - before;
    assert!(
        (BYTES as u64..2 * BYTES as u64).contains(&read),
        "registration read {read} bytes"
    );
    let (status, enabled) = server
        .post(
            "juiced/policy",
            json!({"model":"stored","policy":"on-demand","expected_revision":registered["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    for _ in 0..2 {
        let (status, reply, read) = load(&server, "stored").await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert!(read < QUIET, "a load read {read} bytes");
        unload(&server, "stored").await;
    }
    let path = server
        .directory
        .path()
        .join("artifacts")
        .join(format!("artifact-{artifact}.bin"));
    let entry = remembered(&server, &path);
    assert_eq!(entry["sha256"], artifact, "{entry}");
    assert_eq!(entry["bytes"], BYTES);
    assert_eq!(entry.get("device"), None);

    server.restart().await;
    let started = read_bytes(&server);
    assert!(started < QUIET, "startup read {started} bytes");
    warm(&server).await;
    let (status, reply, read) = load(&server, "stored").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(read < QUIET, "a load after restart read {read} bytes");
    unload(&server, "stored").await;

    std::fs::write(&path, weights(0xa5)).unwrap();
    let (status, refused, read) = load(&server, "stored").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{refused}");
    assert_eq!(refused["error"]["code"], "model_artifact_changed");
    assert!(read >= BYTES as u64, "a changed file was not read again");
    std::fs::write(&path, &original).unwrap();
    let (status, reply, read) = load(&server, "stored").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(read >= BYTES as u64, "a restored file was not read again");
    assert_eq!(remembered(&server, &path)["sha256"], artifact);

    server.stop();
    server.set_environment("JUICED_REMEMBER_CHECKSUMS", "0");
    server.start().await;
    let started = read_bytes(&server);
    assert!(
        started >= BYTES as u64,
        "startup read {started} bytes without the remembered checksum"
    );
}

#[tokio::test]
async fn a_linked_model_is_read_on_first_load_and_again_only_after_it_changes() {
    let library = tempfile::tempdir().unwrap();
    let file = std::fs::canonicalize(library.path())
        .unwrap()
        .join("linked.gguf");
    std::fs::write(&file, weights(0x11)).unwrap();
    let linked = file.clone();
    let mut server = Server::configured(true, move |config| {
        let mut model = config["models"][0].clone();
        model["id"] = json!("linked");
        model["model_file"] = json!(linked);
        model["args"][5] = json!(linked.with_extension("events"));
        config["models"].as_array_mut().unwrap().push(model);
    })
    .await;
    warm(&server).await;

    let (status, reply, read) = load(&server, "linked").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(read >= BYTES as u64, "the first load read {read} bytes");
    let first = fingerprint(&server, "linked").await;
    unload(&server, "linked").await;
    let (status, reply, read) = load(&server, "linked").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(read < QUIET, "the second load read {read} bytes");
    assert_eq!(fingerprint(&server, "linked").await, first);
    assert_eq!(
        remembered(&server, &file)["sha256"],
        format!("{:x}", Sha256::digest(weights(0x11)))
    );

    server.restart().await;
    warm(&server).await;
    let (status, reply, read) = load(&server, "linked").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(read < QUIET, "a load after restart read {read} bytes");
    assert_eq!(fingerprint(&server, "linked").await, first);
    unload(&server, "linked").await;

    std::fs::write(&file, weights(0x22)).unwrap();
    let (status, reply, read) = load(&server, "linked").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(read >= BYTES as u64, "a replaced file was not read again");
    assert_ne!(fingerprint(&server, "linked").await, first);
    assert_eq!(
        remembered(&server, &file)["sha256"],
        format!("{:x}", Sha256::digest(weights(0x22)))
    );
}
