mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::{ADMIN_KEY, CLIENT_KEY, PREFIXES, Server};

async fn configured(model_args: &[&str]) -> Server {
    Server::configured(true, |config| {
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend(model_args.iter().map(|value| json!(value)));
    })
    .await
}

async fn chat(server: &Server, model: &str, stream: bool) -> (StatusCode, Vec<u8>) {
    let response = server
        .request(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":model,"messages":[{"role":"user","content":"private telemetry prompt"}],"stream":stream})),
            server.read_key(),
        )
        .await
        .expect("inference exchange");
    let status = response.status();
    (status, response.bytes().await.unwrap().to_vec())
}

async fn logs(server: &Server, query: &str) -> Value {
    server.get(&format!("juiced/logs{query}")).await
}

async fn wait_for_line(server: &Server, source: &str, needle: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = logs(server, &format!("?source={source}&limit=500")).await;
        if let Some(line) = page["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|line| line["text"].as_str().unwrap().contains(needle))
        {
            return line.clone();
        }
        assert!(
            Instant::now() < deadline,
            "log line {needle:?} did not appear for {source}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn number_or_null(value: &Value, maximum: f64) -> bool {
    value.is_null()
        || value
            .as_f64()
            .is_some_and(|value| (0.0..=maximum).contains(&value))
}

#[tokio::test]
async fn system_stats_requires_read_authority_under_every_prefix_and_keeps_unknowns_null() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        let path = format!("{prefix}/system-stats");
        for key in [None, Some("invalid-key")] {
            let response = server.request(Method::GET, &path, None, key).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        let response = server
            .request(Method::GET, &path, None, Some(CLIENT_KEY))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path} with the inference key"
        );
        let (status, stats) = server.json(Method::GET, &path, None, Some(ADMIN_KEY)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(stats["schema_version"], 1);
        assert!(number_or_null(&stats["cpu_percent"], 100.0));
        assert!(number_or_null(&stats["gpu_percent"], 100.0));
        assert_eq!(stats["npu_percent"], Value::Null);
        assert_eq!(stats["active_requests"], 0);
        assert_eq!(stats["queued_requests"], 0);
        assert_eq!(stats["last_inference"], Value::Null);
        assert_eq!(stats["sample_interval_ms"], 2000);
        for gpu in stats["gpus"].as_array().unwrap() {
            assert!(number_or_null(&gpu["gpu_percent"], 100.0));
            assert!(gpu["id"].as_str().is_some_and(|id| !id.is_empty()));
            assert!(
                gpu["memory_total_bytes"].is_null()
                    || gpu["memory_total_bytes"]
                        .as_u64()
                        .is_some_and(|bytes| bytes > 0)
            );
        }
        if cfg!(target_os = "linux") {
            assert_eq!(stats["supported"], true);
            let memory = &stats["memory"];
            assert_eq!(memory["source"], "proc-meminfo");
            assert!(
                memory["total_bytes"].as_u64().unwrap() >= memory["used_bytes"].as_u64().unwrap()
            );
            assert!(stats["memory_gb"].as_f64().unwrap() > 0.0);
            assert!(stats["sampled_at_ms"].as_u64().unwrap() > 0);
        }
    }
    let first = server.get("system-stats").await["sampled_at_ms"].clone();
    let refreshed = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let stats = server.get("system-stats").await;
            if stats["sampled_at_ms"] != first {
                return stats;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the background sampler publishes a newer sample");
    if cfg!(target_os = "linux") {
        assert!(refreshed["cpu_percent"].as_f64().is_some());
    }
    let head = server
        .request(Method::HEAD, "/api/v1/system-stats", None, Some(ADMIN_KEY))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
}

#[tokio::test]
async fn worker_output_appears_in_operator_logs_with_a_working_cursor() {
    let server = configured(&["--startup-log", "telemetry-marker"]).await;
    let (status, _) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK);
    let stdout = wait_for_line(&server, "fixture.one", "fixture stdout telemetry-marker").await;
    assert_eq!(stdout["stream"], "stdout");
    assert_eq!(stdout["model"], "fixture.one");
    let generation = stdout["generation"].as_u64().expect("worker generation");
    let stderr = wait_for_line(&server, "fixture.one", "fixture stderr telemetry-marker").await;
    assert_eq!(stderr["stream"], "stderr");
    assert_eq!(stderr["level"], "warning");
    assert_eq!(stderr["generation"], generation);
    let long = wait_for_line(&server, "fixture.one", "LLLL").await;
    assert_eq!(long["truncated"], true);
    assert_eq!(long["text"].as_str().unwrap().len(), 4096);
    let started = wait_for_line(&server, "service", "Worker fixture.one generation").await;
    assert_eq!(started["stream"], "service");
    assert_eq!(started["level"], "info");
    assert_eq!(started["model"], Value::Null);

    let page = logs(&server, "?limit=2").await;
    assert_eq!(page["data"].as_array().unwrap().len(), 2);
    assert_eq!(page["dropped"], 0);
    let next = page["next"].as_u64().unwrap();
    assert_eq!(page["data"][1]["seq"], next);
    let rest = logs(&server, &format!("?after={next}&limit=500")).await;
    let sequences: Vec<u64> = rest["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert!(sequences.iter().all(|seq| *seq > next));
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let end = rest["next"].as_u64().unwrap();
    let (status, _) = chat(&server, "fixture.one", false).await;
    assert_eq!(status, StatusCode::OK);
    let after_inference = logs(&server, &format!("?after={end}&limit=500")).await;
    assert!(
        !after_inference
            .to_string()
            .contains("private telemetry prompt")
    );
    assert!(
        after_inference["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|line| line["seq"].as_u64().unwrap() > end)
    );

    for (query, code) in [
        ("?limit=0", "invalid_log_limit"),
        ("?limit=501", "invalid_log_limit"),
        ("?source=", "invalid_log_source"),
    ] {
        let (status, value) = server
            .json(
                Method::GET,
                &format!("/v1/juiced/logs{query}"),
                None,
                Some(ADMIN_KEY),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(value["error"]["code"], code);
    }
    let (status, _) = server
        .json(
            Method::GET,
            "/v1/juiced/logs?cursor=1",
            None,
            Some(ADMIN_KEY),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for prefix in PREFIXES {
        let response = server
            .request(
                Method::GET,
                &format!("{prefix}/juiced/logs"),
                None,
                Some(CLIENT_KEY),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = server
            .request(
                Method::GET,
                &format!("{prefix}/juiced/logs?limit=1"),
                None,
                Some(ADMIN_KEY),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn a_chatty_worker_cannot_stall_inference_and_old_lines_are_reported_dropped() {
    let server = configured(&["--chatty-bytes", "2097152"]).await;
    for _ in 0..3 {
        let started = Instant::now();
        let (status, body) = chat(&server, "fixture.one", false).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "inference waited on worker output"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = 0;
    let page = loop {
        let page = logs(&server, "?limit=1").await;
        let latest = page["latest"].as_u64().unwrap();
        if latest > 40_000 && latest == previous {
            break page;
        }
        previous = latest;
        assert!(Instant::now() < deadline, "worker output was not drained");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(page["dropped"].as_u64().unwrap() > 0);
    let oldest = page["oldest"].as_u64().unwrap();
    assert_eq!(page["data"][0]["seq"], oldest);
    let retained = page["latest"].as_u64().unwrap() - oldest + 1;
    assert!(retained <= 4096);
    let tail = logs(&server, &format!("?after={}&limit=500", oldest - 1)).await;
    assert_eq!(tail["dropped"], 0);
    let health = server.get("health").await;
    assert_eq!(health["status"], "ok");
}

#[tokio::test]
async fn last_decode_rate_follows_completed_inference_without_content() {
    let server = configured(&["--report-timings", "--finite-stream"]).await;
    assert_eq!(
        server.get("system-stats").await["last_inference"],
        Value::Null
    );

    let (status, _) = chat(&server, "fixture.one", false).await;
    assert_eq!(status, StatusCode::OK);
    let last = server.get("system-stats").await["last_inference"].clone();
    assert_eq!(last["model"], "fixture.one");
    assert_eq!(last["operation"], "chat/completions");
    assert_eq!(last["decode_tokens_per_second"], 42.5);
    assert_eq!(last["prompt_tokens_per_second"], 700.0);
    assert_eq!(last["input_tokens"], 7);
    assert_eq!(last["output_tokens"], 2);
    assert!(last["completed_at_ms"].as_u64().unwrap() > 0);

    let (status, body) = chat(&server, "fixture.one", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8(body).unwrap().contains("[DONE]"));
    let last = server.get("system-stats").await["last_inference"].clone();
    assert_eq!(last["decode_tokens_per_second"], 37.5);
    assert_eq!(last["prompt_tokens_per_second"], 350.0);
    assert_eq!(last["output_tokens"], 2);

    let (status, _) = chat(&server, "fixture.two", false).await;
    assert_eq!(status, StatusCode::OK);
    let stats = server.get("system-stats").await;
    assert_eq!(stats["last_inference"]["model"], "fixture.two");
    assert_eq!(
        stats["last_inference"]["decode_tokens_per_second"],
        Value::Null
    );
    assert_eq!(stats["last_inference"]["input_tokens"], 7);
    assert_eq!(stats["active_requests"], 0);

    let history = server.get("juiced/requests").await;
    let records = history["data"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["decode_tokens_per_second"], Value::Null);
    assert_eq!(records[1]["decode_tokens_per_second"], 37.5);
    assert_eq!(records[2]["decode_tokens_per_second"], 42.5);
    let encoded = history.to_string() + &stats.to_string();
    assert!(!encoded.contains("private telemetry prompt"));
    assert!(!encoded.contains("Rust fixture reply"));

    let (status, _) = server
        .json(
            Method::POST,
            "/v1/chat/completions",
            Some(json!({"model":"fixture.one","messages":[],"fixture_status":500})),
            server.read_key(),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        server.get("system-stats").await["last_inference"]["model"],
        "fixture.two"
    );
}
