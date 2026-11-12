mod support;

use serde_json::Value;
use std::time::Duration;
use support::{CLIENT_KEY, Server};

#[tokio::test]
async fn benchmark_reports_completed_requests_and_redacts_credentials() {
    let server = Server::new(true).await;
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_bench-api"))
        .args([
            "--url",
            &server.base,
            "--path",
            "/v1/health",
            "--requests",
            "32",
            "--concurrency",
            "4",
            "--pid",
            &server.pid().to_string(),
        ])
        .env_remove("LEMONADE_ADMIN_API_KEY")
        .env("LEMONADE_API_KEY", CLIENT_KEY)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "benchmark smoke failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["workload"]["requests"], 32);
    assert_eq!(result["workload"]["concurrency"], 4);
    assert_eq!(result["workload"]["authenticated"], true);
    assert_eq!(result["successful"], 32);
    assert_eq!(result["errors"], 0);
    assert!(result["latency_us"]["p50"].as_f64().unwrap() > 0.0);
    assert!(
        result["latency_us"]["p99"].as_f64().unwrap()
            >= result["latency_us"]["p50"].as_f64().unwrap()
    );
    assert!(result["successful_requests_per_second"].as_f64().unwrap() > 0.0);
    #[cfg(target_os = "linux")]
    assert!(
        result["process"]["peak_sampled_rss_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(CLIENT_KEY));
}
