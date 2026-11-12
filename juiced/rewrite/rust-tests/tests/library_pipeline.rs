mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::{
    CLIENT_KEY, PREFIXES, Server,
    huggingface::{Hub, REVISION},
};

const TOKEN: &str = "process-hub-token-never-returned";
const SINGLE: &str = "fixture/Process-GGUF";
const SHARDED: &str = "fixture/Shard-GGUF";

async fn server(hub: &Hub, policy: &str) -> Server {
    let mut server = Server::prepare(true, |config| {
        config["artifact_source_policy"] = json!(policy)
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.set_environment("HF_TOKEN", TOKEN);
    server.start().await;
    server
}

async fn search(server: &Server, prefix: &str, query: &str) -> Value {
    let (status, value) = server
        .json(
            Method::GET,
            &format!("{prefix}/juiced/library/search?{query}"),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(!value.to_string().contains(TOKEN));
    value
}

fn entry<'a>(result: &'a Value, id: &str) -> &'a Value {
    result["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {result}"))
}

fn quant<'a>(entry: &'a Value, file: &str) -> &'a Value {
    entry["quants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|quant| quant["file"] == file)
        .unwrap_or_else(|| panic!("{file} missing from {entry}"))
}

async fn settle(server: &Server, prefix: &str, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (status, view) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/library/pulls/{id}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        if matches!(view["state"].as_str(), Some("complete" | "failed")) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "library pull did not settle: {view}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn search_pull_and_verified_artifacts_work_under_every_prefix() {
    let hub = Hub::start().await;
    let mut server = server(&hub, "allow-loopback-http").await;
    let files = [
        "Process-Q4_K_M.gguf",
        "Process-Q8_0.gguf",
        "Process-IQ4_NL.gguf",
        "Process-MXFP4.gguf",
    ];
    for (prefix, file) in PREFIXES.iter().zip(files) {
        for key in [None, Some(CLIENT_KEY), Some("invalid-key")] {
            let (status, _) = server
                .json(
                    Method::GET,
                    &format!("{prefix}/juiced/library/search?q=process"),
                    None,
                    key,
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            let (status, _) = server
                .json(
                    Method::POST,
                    &format!("{prefix}/juiced/library/pull"),
                    Some(json!({"repo": SINGLE, "file": file, "expected_generation": 0})),
                    key,
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let result = search(&server, prefix, "q=process").await;
        assert_eq!(result["sources"]["huggingface"]["status"], "ok");
        let repo = entry(&result, SINGLE);
        assert_eq!(repo["source"], "huggingface");
        assert_eq!(repo["revision"], REVISION);
        assert_eq!(repo["license"], "apache-2.0");
        let selected = quant(repo, file);
        let expected = hub.file(SINGLE, file);
        assert_eq!(selected["sha256"], expected.sha256());
        assert_eq!(selected["bytes"], expected.bytes.len());
        assert_eq!(selected["pullable"], true);
        assert_eq!(selected["local"]["present"], false);
        let (status, started) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/library/pull"),
                Some(json!({"repo": SINGLE, "file": file, "expected_generation": result["artifact_generation"]})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{started}");
        assert_eq!(started["revision"], REVISION);
        assert_eq!(
            started["files"][0]["source_url"],
            format!("{}/{SINGLE}/resolve/{REVISION}/{file}", hub.base)
        );
        let done = settle(&server, prefix, started["pull"].as_str().unwrap()).await;
        assert_eq!(done["state"], "complete", "{done}");
        let job = done["files"][0]["job"].as_str().unwrap();
        let (status, job) = server
            .json(
                Method::GET,
                &format!("{prefix}/juiced/artifacts/jobs/{job}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(job["state"], "complete");
        assert_eq!(job["artifact"], expected.sha256());
        assert_eq!(job["source_url"], started["files"][0]["source_url"]);
        let marked = search(&server, prefix, "q=process").await;
        let marked = quant(entry(&marked, SINGLE), file);
        assert_eq!(marked["local"]["present"], true);
    }
    let inventory = server.get("juiced/artifacts").await;
    let artifacts: Vec<&str> = inventory["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| artifact["sha256"].as_str().unwrap())
        .collect();
    for file in files {
        assert!(artifacts.contains(&hub.file(SINGLE, file).sha256().as_str()));
    }

    let result = search(&server, "/v1", "q=shard").await;
    let shard = quant(entry(&result, SHARDED), "Shard-Q4_K_M-00001-of-00002.gguf");
    assert_eq!(shard["parts"], 2);
    assert_eq!(shard["bytes"], 50_000);
    let (status, started) = server
        .post(
            "juiced/library/pull",
            json!({"repo": SHARDED, "files": ["Shard-Q4_K_M-00002-of-00002.gguf"], "revision": REVISION, "expected_generation": result["artifact_generation"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let set = started["set"].as_str().unwrap().to_owned();
    let done = settle(&server, "/v1", started["pull"].as_str().unwrap()).await;
    assert_eq!(done["set_state"], "published", "{done}");
    let published = server.get(&format!("juiced/artifacts/sets/{set}")).await;
    assert_eq!(published["members_present"], true);
    assert_eq!(
        published["specification"]["primary"],
        "Shard-Q4_K_M-00001-of-00002.gguf"
    );
    assert_eq!(
        published["specification"]["origin"]["source_revision"],
        REVISION
    );
    let roles: Vec<&str> = published["specification"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["model", "weight-shard"]);

    let observed = hub.observed();
    for request in &observed {
        if request.path.starts_with("/api/") {
            assert_eq!(
                request.authorization.as_deref(),
                Some(format!("Bearer {TOKEN}").as_str())
            );
        } else {
            assert_eq!(request.authorization, None, "{}", request.path);
        }
    }
    assert!(
        observed
            .iter()
            .any(|request| request.path.contains("/resolve/"))
    );

    server.restart().await;
    let offline = search(&server, "/v1", "q=shard").await;
    let shard = quant(entry(&offline, SHARDED), "Shard-Q4_K_M-00001-of-00002.gguf");
    assert_eq!(shard["local"]["present"], true);
    assert_eq!(shard["local"]["set"], set);
}

#[tokio::test]
async fn offline_and_blocked_policies_serve_catalog_and_local_results_only() {
    let hub = Hub::start().await;
    let server = server(&hub, "offline").await;
    let before = hub.observed().len();
    for prefix in PREFIXES {
        let result = search(&server, prefix, "q=qwen3&task=chat&limit=5").await;
        assert_eq!(result["offline"], true);
        assert_eq!(result["sources"]["huggingface"]["status"], "offline");
        assert_eq!(result["page"]["next_cursor"], Value::Null);
        let data = result["data"].as_array().unwrap();
        assert!(!data.is_empty() && data.len() <= 5);
        assert!(data.iter().all(|entry| entry["source"] == "catalog"));
        assert!(data.iter().all(|entry| entry["quants"].is_null()));
        let (status, error) = server
            .json(
                Method::POST,
                &format!("{prefix}/juiced/library/pull"),
                Some(
                    json!({"repo": SINGLE, "file": "Process-Q8_0.gguf", "expected_generation": 0}),
                ),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["error"]["code"], "artifact_offline");
    }
    assert_eq!(hub.observed().len(), before);
    let public = server_with_default_policy(&hub).await;
    let result = search(&public, "/v1", "q=whisper&task=transcription").await;
    assert_eq!(result["sources"]["huggingface"]["status"], "blocked");
    assert_eq!(
        entry(&result, "whisper-tiny")["recommended_file"],
        "ggml-tiny.bin"
    );
    for query in [
        "limit=0",
        "limit=21",
        "cursor=%20",
        "task=vision",
        "unexpected=1",
    ] {
        let (status, _) = public
            .json(
                Method::GET,
                &format!("/v1/juiced/library/search?{query}"),
                None,
                public.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
    assert_eq!(hub.observed().len(), before);
}

async fn server_with_default_policy(hub: &Hub) -> Server {
    let mut server = Server::prepare(true, |_| {});
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.start().await;
    server
}
