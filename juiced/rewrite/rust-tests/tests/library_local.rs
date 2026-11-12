mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use support::{CLIENT_KEY, PREFIXES, Server, metadata_gguf};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

/// Writes `models--owner--name` with content-addressed blobs and snapshot links, the layout
/// `huggingface_hub` leaves behind.
fn cache_repo(hub: &Path, repo: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
    let directory = hub.join(format!("models--{}", repo.replace('/', "--")));
    let snapshot = directory.join("snapshots").join(REVISION);
    std::fs::create_dir_all(directory.join("blobs")).unwrap();
    std::fs::create_dir_all(directory.join("refs")).unwrap();
    std::fs::write(directory.join("refs/main"), REVISION).unwrap();
    for (name, bytes) in files {
        let digest = format!("{:x}", Sha256::digest(bytes));
        let blob = directory.join("blobs").join(&digest);
        std::fs::write(&blob, bytes).unwrap();
        let link = snapshot.join(name);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let depth = name.matches('/').count();
        let target = format!("{}../../blobs/{digest}", "../".repeat(depth));
        std::os::unix::fs::symlink(target, &link).unwrap();
    }
    snapshot
}

fn group<'a>(inventory: &'a Value, id: &str) -> &'a Value {
    inventory["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {inventory}"))
}

fn file<'a>(group: &'a Value, name: &str) -> &'a Value {
    group["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {group}"))
}

fn quant<'a>(group: &'a Value, label: &str) -> &'a Value {
    group["quants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|quant| quant["label"] == label)
        .unwrap_or_else(|| panic!("{label} missing from {group}"))
}

fn writable_by_us(directory: &Path) -> bool {
    let probe = directory.join(".probe");
    let writable = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(probe);
    writable
}

#[tokio::test]
async fn local_inventory_groups_cache_store_and_in_place_files_and_guards_deletion() {
    let library = tempfile::tempdir().unwrap();
    let mut server = Server::prepare(true, |_| {});
    let owned = server.directory.path().join("huggingface/hub");
    let tiny = cache_repo(
        &owned,
        "acme/Tiny-GGUF",
        &[
            ("Tiny-Q4_K_M.gguf", metadata_gguf(512)),
            ("Tiny-Q8_0.gguf", metadata_gguf(1024)),
            ("mmproj-Tiny-F16.gguf", b"projector bytes".to_vec()),
            ("README.md", b"# not listed".to_vec()),
        ],
    );
    let mounted = library.path().join("huggingface/hub");
    let big = cache_repo(
        &mounted,
        "acme/Big-GGUF",
        &[
            ("Big-Q4_0-00001-of-00002.gguf", b"part one".to_vec()),
            ("Big-Q4_0-00002-of-00002.gguf", b"part two".to_vec()),
        ],
    );
    let loose = library.path().join("asr/models");
    std::fs::create_dir_all(&loose).unwrap();
    std::fs::write(loose.join("speech-Q8_0.gguf"), b"loose speech weights").unwrap();

    let worker = support::binary("juiced-test-worker", "JUICED_TEST_WORKER");
    let registration = |id: &str, model_file: PathBuf| {
        json!({
            "id": id, "model_file": model_file, "command": worker,
            "args": ["--port", "{port}", "--model", "{model}"],
            "resident_bytes": 64, "max_inflight": 1, "operations": ["chat/completions"],
            "policy": "disabled",
        })
    };
    server.config["models"] = json!([
        registration("tiny.q8", tiny.join("Tiny-Q8_0.gguf")),
        registration("speech", loose.join("speech-Q8_0.gguf")),
    ]);
    server.config["import_roots"] = json!([library.path()]);
    server.config["upload_max_bytes"] = json!(1024 * 1024);
    std::fs::write(
        &server.config_path,
        serde_json::to_vec_pretty(&server.config).unwrap(),
    )
    .unwrap();
    let mut mode = std::fs::metadata(&mounted).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o555);
    std::fs::set_permissions(&mounted, mode).unwrap();
    let read_only_enforced = !writable_by_us(&mounted);
    server.start().await;

    for prefix in PREFIXES {
        for key in [None, Some(CLIENT_KEY)] {
            let (status, _) = server
                .json(
                    Method::GET,
                    &format!("{prefix}/juiced/library/local"),
                    None,
                    key,
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{prefix}");
            let (status, _) = server
                .json(
                    Method::POST,
                    &format!("{prefix}/juiced/library/local/delete"),
                    Some(json!({"files": ["x"]})),
                    key,
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{prefix}");
        }
    }

    let uploaded = metadata_gguf(256);
    let upload = server
        .client
        .post(format!(
            "{}/v1/juiced/library/upload?filename=Upload-Q6_K.gguf&size={}",
            server.base,
            uploaded.len()
        ))
        .bearer_auth(support::ADMIN_KEY)
        .body(uploaded.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);
    let artifact = format!("{:x}", Sha256::digest(&uploaded));

    let inventory = server.get("juiced/library/local").await;
    assert_eq!(inventory["schema_version"], 1);
    let hub = group(&inventory, "hf:acme/Tiny-GGUF");
    assert_eq!(hub["name"], "Tiny-GGUF");
    assert_eq!(hub["publisher"], "acme");
    assert_eq!(hub["storage"], json!(["huggingface_cache"]));
    assert_eq!(hub["read_only"], false);
    assert_eq!(hub["used_by"], json!(["tiny.q8"]));
    assert!(
        hub["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["name"] != "README.md")
    );
    let q4 = file(hub, "Tiny-Q4_K_M.gguf");
    assert_eq!(q4["quant"], "Q4_K_M");
    assert_eq!(q4["role"], "model");
    assert_eq!(q4["revision"], REVISION);
    assert_eq!(q4["bytes"], metadata_gguf(512).len());
    assert_eq!(q4["removable"], true);
    assert_eq!(q4["refusal"], Value::Null);
    let q8 = file(hub, "Tiny-Q8_0.gguf");
    assert_eq!(q8["used_by"], json!(["tiny.q8"]));
    assert_eq!(q8["refusal"], "in_use");
    assert_eq!(file(hub, "mmproj-Tiny-F16.gguf")["role"], "projector");
    assert_eq!(
        quant(hub, "Q4_K_M")["add"],
        json!({"path": tiny.join("Tiny-Q4_K_M.gguf")})
    );
    assert_eq!(quant(hub, "Q8_0")["used_by"], json!(["tiny.q8"]));
    assert_eq!(
        hub["bytes"],
        metadata_gguf(512).len() + metadata_gguf(1024).len() + b"projector bytes".len()
    );

    let mounted_group = group(&inventory, "hf:acme/Big-GGUF");
    let split = quant(mounted_group, "Q4_0");
    assert_eq!(split["parts"], 2);
    assert_eq!(split["complete"], true);
    assert_eq!(split["add"], Value::Null);
    assert_eq!(
        file(mounted_group, "Big-Q4_0-00001-of-00002.gguf")["role"],
        "shard"
    );
    if read_only_enforced {
        assert_eq!(mounted_group["read_only"], true);
        assert_eq!(
            file(mounted_group, "Big-Q4_0-00002-of-00002.gguf")["refusal"],
            "read_only"
        );
    }

    let in_place = inventory["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["name"] == "speech")
        .unwrap_or_else(|| panic!("in-place group missing from {inventory}"));
    assert_eq!(in_place["storage"], json!(["local"]));
    assert_eq!(in_place["used_by"], json!(["speech"]));
    assert_eq!(file(in_place, "speech-Q8_0.gguf")["refusal"], "in_use");

    let stored = inventory["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["storage"] == json!(["artifact_store"]))
        .unwrap_or_else(|| panic!("artifact-store group missing from {inventory}"));
    assert_eq!(stored["name"], "Upload");
    let upload_file = file(stored, "Upload-Q6_K.gguf");
    assert_eq!(upload_file["id"], format!("artifact:{artifact}"));
    assert_eq!(upload_file["removable"], true);
    assert_eq!(quant(stored, "Q6_K")["add"], json!({"artifact": artifact}));

    let search = server.get("juiced/library/search?q=tiny&hub=false").await;
    let entry = search["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["repo"] == "acme/Tiny-GGUF")
        .unwrap_or_else(|| panic!("cache repo missing from search {search}"));
    let found = entry["quants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|quant| quant["label"] == "Q4_K_M")
        .unwrap();
    assert_eq!(found["local"]["present"], true, "{found}");
    assert_eq!(found["local"]["storage"], "huggingface_cache");
    assert_eq!(found["local"]["path"], json!(tiny.join("Tiny-Q4_K_M.gguf")));

    let refusals = [
        (q8["id"].clone(), StatusCode::CONFLICT, "local_file_in_use"),
        (
            json!("/etc/hostname"),
            StatusCode::NOT_FOUND,
            "local_file_not_found",
        ),
        (
            file(in_place, "speech-Q8_0.gguf")["id"].clone(),
            StatusCode::CONFLICT,
            "local_file_in_use",
        ),
    ];
    for (id, status, code) in refusals {
        let (observed, reply) = server
            .post("juiced/library/local/delete", json!({"files": [id]}))
            .await;
        assert_eq!(observed, status, "{reply}");
        assert_eq!(reply["error"]["code"], code);
    }
    if read_only_enforced {
        let (status, reply) = server
            .post(
                "juiced/library/local/delete",
                json!({"files": [file(mounted_group, "Big-Q4_0-00001-of-00002.gguf")["id"]]}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{reply}");
        assert_eq!(reply["error"]["code"], "local_file_read_only");
    }
    let (status, reply) = server
        .post("juiced/library/local/delete", json!({"files": []}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(tiny.join("Tiny-Q8_0.gguf").exists());

    let blob = std::fs::canonicalize(tiny.join("Tiny-Q4_K_M.gguf")).unwrap();
    let (status, reply) = server
        .post(
            "juiced/library/local/delete",
            json!({"files": [q4["id"], upload_file["id"]]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        reply["freed_bytes"],
        metadata_gguf(512).len() + uploaded.len()
    );
    assert!(!tiny.join("Tiny-Q4_K_M.gguf").exists());
    assert!(!blob.exists());
    assert!(tiny.join("Tiny-Q8_0.gguf").exists());
    let artifacts = server.get("juiced/artifacts").await;
    assert!(
        artifacts["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != artifact),
        "{artifacts}"
    );

    let after = server.get("juiced/library/local").await;
    let hub = group(&after, "hf:acme/Tiny-GGUF");
    assert!(
        hub["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["name"] != "Tiny-Q4_K_M.gguf")
    );
    assert!(
        after["groups"]
            .as_array()
            .unwrap()
            .iter()
            .all(|group| group["storage"] != json!(["artifact_store"])),
        "{after}"
    );

    let mut mode = std::fs::metadata(&mounted).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&mounted, mode).unwrap();
    let _ = big;
}

#[tokio::test]
async fn library_pulls_list_and_dismissal_refuse_unknown_ids() {
    let server = Server::new(true).await;
    let pulls = server.get("juiced/library/pulls").await;
    assert_eq!(pulls, json!({"schema_version": 1, "data": []}));
    let (status, reply) = server
        .post("juiced/library/pulls/missing", json!({"action": "dismiss"}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{reply}");
    assert_eq!(reply["error"]["code"], "library_pull_not_found");
    let (status, reply) = server
        .post("juiced/library/pulls/missing", json!({"action": "pause"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
}
