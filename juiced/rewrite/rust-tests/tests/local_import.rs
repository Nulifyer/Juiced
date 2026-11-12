mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use support::{ADMIN_KEY, CLIENT_KEY, PREFIXES, Server, metadata_gguf};
use tokio::io::AsyncWriteExt;

const UPLOAD_LIMIT: u64 = 64 * 1024 * 1024;

fn weights() -> Vec<u8> {
    std::env::var_os("JUICED_TEST_GGUF")
        .map(|path| std::fs::read(path).expect("JUICED_TEST_GGUF names a readable file"))
        .unwrap_or_else(|| metadata_gguf(2048))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn staged_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "part")
            {
                found.push(path);
            }
        }
    }
    found
}

async fn upload(server: &Server, name: &str, declared: u64, body: Vec<u8>) -> (StatusCode, Value) {
    let response = server
        .client
        .post(format!(
            "{}/v1/juiced/library/upload?filename={name}&size={declared}",
            server.base
        ))
        .bearer_auth(ADMIN_KEY)
        .header("content-type", "application/octet-stream")
        .body(body)
        .send()
        .await
        .expect("upload exchange");
    let status = response.status();
    (status, response.json().await.expect("JSON upload reply"))
}

async fn import_server() -> (Server, tempfile::TempDir, tempfile::TempDir) {
    let imports = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = imports.path().to_owned();
    let server = Server::configured(true, |config| {
        config["models"] = json!([]);
        config["upload_max_bytes"] = json!(UPLOAD_LIMIT);
        config["import_roots"] = json!([root]);
    })
    .await;
    (server, imports, outside)
}

async fn settled_job(server: &Server, id: &str) -> Value {
    for _ in 0..400 {
        let job = server.get(&format!("juiced/artifacts/jobs/{id}")).await;
        if job["state"] == "complete" || job["state"] == "failed" {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("import job did not settle");
}

async fn no_open_jobs(server: &Server) {
    for _ in 0..200 {
        let inventory = server.get("juiced/artifacts?section=jobs").await;
        let open = inventory["jobs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|job| job["state"] != "complete")
            .count();
        if open == 0 && staged_files(server.directory.path()).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "partial uploads remain: {:?} {}",
        staged_files(server.directory.path()),
        server.get("juiced/artifacts?section=jobs").await
    );
}

#[tokio::test]
async fn uploads_publish_dedupe_and_reject_without_keeping_partial_bytes() {
    let (server, _imports, _outside) = import_server().await;
    let bytes = weights();
    let sha = digest(&bytes);
    for prefix in PREFIXES {
        for key in [None, Some(CLIENT_KEY)] {
            let mut request = server.client.post(format!(
                "{}{prefix}/juiced/library/upload?filename=a.gguf&size=4",
                server.base
            ));
            if let Some(key) = key {
                request = request.bearer_auth(key);
            }
            let response = request.body("GGUF").send().await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{prefix}");
        }
    }

    let (status, first) = upload(
        &server,
        "Stories.Q4_0.gguf",
        bytes.len() as u64,
        bytes.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["artifact"], sha);
    assert_eq!(first["sha256"], sha);
    assert_eq!(first["bytes"], bytes.len());
    assert_eq!(first["file"], "Stories.Q4_0.gguf");
    assert_eq!(first["duplicate"], false);
    assert_eq!(first["format"]["format"], "gguf");
    assert_eq!(first["suggested_id"], "Stories.Q4_0");
    assert_eq!(first["add"], json!({"artifact":sha,"id":"Stories.Q4_0"}));
    assert_eq!(first["engines"][0]["engine"], "llama.cpp");
    assert!(first["engines"][0]["ready"].is_boolean());
    let job = server
        .get(&format!(
            "juiced/artifacts/jobs/{}",
            first["job"].as_str().unwrap()
        ))
        .await;
    assert_eq!(job["state"], "complete");
    assert_eq!(job["artifact"], sha);
    assert_eq!(job["source_name"], "Stories.Q4_0.gguf");
    assert_eq!(job["computed_digest"], true);

    let (status, again) = upload(&server, "renamed.gguf", bytes.len() as u64, bytes.clone()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["artifact"], sha);
    assert_eq!(again["duplicate"], true);
    let inventory = server.get("juiced/artifacts?section=artifacts").await;
    assert_eq!(
        inventory["artifacts"].as_array().unwrap().len(),
        1,
        "{inventory}"
    );

    for (name, declared, body, status, code) in [
        (
            "huge.gguf",
            UPLOAD_LIMIT + 1,
            b"GGUF".to_vec(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload_too_large",
        ),
        (
            "text.gguf",
            26,
            b"this is not a model at all".to_vec(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_model_format",
        ),
        (
            "short.gguf",
            64,
            b"GGUF".to_vec(),
            StatusCode::BAD_REQUEST,
            "upload_size_mismatch",
        ),
        (
            "../escape.gguf",
            4,
            b"GGUF".to_vec(),
            StatusCode::BAD_REQUEST,
            "invalid_upload_name",
        ),
        (
            "model-00001-of-00002.gguf",
            4,
            b"GGUF".to_vec(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "split_model_unsupported",
        ),
        (
            "empty.gguf",
            0,
            Vec::new(),
            StatusCode::BAD_REQUEST,
            "upload_size_mismatch",
        ),
    ] {
        let (actual, refused) = upload(&server, name, declared, body).await;
        assert_eq!(actual, status, "{name}: {refused}");
        assert_eq!(refused["error"]["code"], code, "{name}");
    }
    let mut truncated = metadata_gguf(2048);
    truncated.truncate(24);
    let (status, refused) = upload(&server, "truncated.gguf", 24, truncated).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    no_open_jobs(&server).await;
    assert_eq!(
        server.get("juiced/artifacts?section=artifacts").await["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn aborted_uploads_remove_their_staging_file_and_job() {
    let (server, _imports, _outside) = import_server().await;
    let address = server.base.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let declared = 8 * 1024 * 1024;
    socket
        .write_all(
            format!(
                "POST /v1/juiced/library/upload?filename=aborted.gguf&size={declared} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {ADMIN_KEY}\r\nContent-Type: application/octet-stream\r\nContent-Length: {declared}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut body = metadata_gguf(2048);
    body.resize(5 * 1024 * 1024, 0);
    socket.write_all(&body).await.unwrap();
    socket.flush().await.unwrap();
    server
        .wait_for(async || {
            server.get("juiced/artifacts?section=jobs").await["jobs"]
                .as_array()
                .is_some_and(|jobs| jobs.iter().any(|job| job["state"] == "downloading"))
        })
        .await;
    drop(socket);
    no_open_jobs(&server).await;
    assert!(
        server.get("juiced/artifacts?section=artifacts").await["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn import_path_lists_copies_and_links_only_inside_import_roots() {
    let (server, imports, outside) = import_server().await;
    let root = std::fs::canonicalize(imports.path()).unwrap();
    let bytes = weights();
    let sha = digest(&bytes);
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/model.gguf"), &bytes).unwrap();
    std::fs::write(root.join("notes.txt"), b"ignored").unwrap();
    std::fs::write(outside.path().join("secret.gguf"), &bytes).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.gguf"), root.join("escape.gguf"))
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("escape-dir")).unwrap();

    let listing = server.get("juiced/library/import-path").await;
    assert_eq!(listing["enabled"], true);
    assert_eq!(listing["roots"][0]["available"], true);
    assert_eq!(listing["entries"][0]["path"], json!(root));
    let listing = server
        .get(&format!(
            "juiced/library/import-path?dir={}",
            root.display()
        ))
        .await;
    let names: Vec<_> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["nested"], "{listing}");
    let nested = server
        .get(&format!(
            "juiced/library/import-path?dir={}/nested",
            root.display()
        ))
        .await;
    assert_eq!(nested["parent"], json!(root));
    assert_eq!(nested["entries"][0]["name"], "model.gguf");
    assert_eq!(nested["entries"][0]["kind"], "file");
    assert_eq!(nested["entries"][0]["bytes"], bytes.len());
    let (status, refused) = server
        .json(
            Method::GET,
            &format!(
                "/v1/juiced/library/import-path?dir={}",
                outside.path().display()
            ),
            None,
            Some(ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");

    for (path, code) in [
        (
            outside.path().join("secret.gguf"),
            "import_path_outside_roots",
        ),
        (root.join("escape.gguf"), "import_path_outside_roots"),
        (
            root.join("escape-dir/secret.gguf"),
            "import_path_outside_roots",
        ),
        (root.join("nested/../escape.gguf"), "invalid_import_path"),
        (root.join("missing.gguf"), "import_path_not_found"),
        (root.join("nested"), "import_path_not_file"),
        (root.join("notes.txt"), "unsupported_model_format"),
    ] {
        let (status, refused) = server
            .post(
                "juiced/library/import-path",
                json!({"path":path,"mode":"copy"}),
            )
            .await;
        assert!(status.is_client_error(), "{}: {refused}", path.display());
        assert_eq!(refused["error"]["code"], code, "{}", path.display());
    }

    let (status, copied) = server
        .post(
            "juiced/library/import-path",
            json!({"path":root.join("nested/model.gguf"),"mode":"copy"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{copied}");
    assert_eq!(copied["mode"], "copy");
    assert_eq!(copied["add"], Value::Null);
    assert_eq!(copied["job"]["expected_bytes"], bytes.len());
    assert_eq!(copied["job"]["computed_digest"], true);
    let job = settled_job(&server, copied["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "complete", "{job}");
    assert_eq!(job["artifact"], sha);
    assert_eq!(job["downloaded_bytes"], bytes.len());
    assert_eq!(job["source_name"], "model.gguf");
    assert!(root.join("nested/model.gguf").is_file());

    let (status, linked) = server
        .post(
            "juiced/library/import-path",
            json!({"path":root.join("nested/model.gguf"),"mode":"link"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["job"], Value::Null);
    assert_eq!(
        linked["add"],
        json!({"path":root.join("nested/model.gguf"),"id":"model"})
    );
    assert_eq!(linked["format"]["format"], "gguf");
    let (status, refused) = server
        .post(
            "juiced/models/add",
            json!({"path":root.join("escape.gguf"),"id":"escape"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "import_path_outside_roots");
}

#[tokio::test]
async fn import_path_is_disabled_without_configured_roots() {
    let server = Server::new(true).await;
    let listing = server.get("juiced/library/import-path").await;
    assert_eq!(listing["enabled"], false);
    assert_eq!(listing["entries"], json!([]));
    let (status, refused) = server
        .post(
            "juiced/library/import-path",
            json!({"path":"/etc/hostname","mode":"link"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "import_roots_unconfigured");
    let (status, refused) = server
        .post("juiced/models/add", json!({"path":"/etc/hostname"}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "import_roots_unconfigured");
}
