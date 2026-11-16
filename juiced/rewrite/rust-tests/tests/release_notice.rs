mod support;

use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::Server;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn stub_release(
    tag: &'static str,
) -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let head = Arc::new(std::sync::Mutex::new(String::new()));
    let (counted, seen) = (hits.clone(), head.clone());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = vec![0; 8192];
            let read = socket.read(&mut request).await.unwrap_or(0);
            *seen.lock().unwrap() = String::from_utf8_lossy(&request[..read]).into_owned();
            counted.fetch_add(1, Ordering::SeqCst);
            let body = format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/Nulifyer/Juiced/releases/tag/{tag}"}}"#
            );
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    (
        format!("http://{address}/repos/Nulifyer/Juiced/releases/latest"),
        hits,
        head,
    )
}

async fn release(server: &Server) -> Value {
    server.get("juiced/state").await["release"].clone()
}

#[tokio::test]
async fn the_release_notice_reports_a_newer_release_once_and_turns_off() {
    let (url, hits, head) = stub_release("v999.0.0").await;
    let mut server = Server::prepare(true, |_| {});
    server.set_environment("JUICED_TEST_RELEASES_URL", &url);
    server.start().await;
    server
        .wait_for(async || release(&server).await["latest"] == "v999.0.0")
        .await;
    let notice = release(&server).await;
    assert_eq!(notice["check"], true);
    assert_eq!(notice["newer"], true);
    assert_eq!(notice["current"], juiced_service::VERSION);
    assert_eq!(
        notice["url"],
        "https://github.com/Nulifyer/Juiced/releases/tag/v999.0.0"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let request = head.lock().unwrap().to_lowercase();
    assert!(request.starts_with("get /repos/nulifyer/juiced/releases/latest "));
    assert!(request.contains("\r\nuser-agent: juiced\r\n"), "{request}");
    for private in ["authorization", "cookie", "x-api-key", "hostname"] {
        assert!(!request.contains(private), "{request}");
    }

    let (status, body) = server
        .post("juiced/release", json!({"enabled": false}))
        .await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["release"]["check"], false);
    assert_eq!(body["release"]["latest"], Value::Null);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(saved["release_check"], false);

    server.restart().await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(release(&server).await["check"], false);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let (status, body) = server
        .post("juiced/release", json!({"enabled": true}))
        .await;
    assert!(status.is_success(), "{body}");
    server
        .wait_for(async || release(&server).await["newer"] == true)
        .await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert!(saved.get("release_check").is_none());

    let (status, _) = server
        .post("juiced/release", json!({"enabled": "no"}))
        .await;
    assert_eq!(status, reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn an_unreachable_release_source_stays_silent() {
    let server = Server::new(false).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let notice = release(&server).await;
    assert_eq!(notice["check"], true);
    assert_eq!(notice["latest"], Value::Null);
    assert_eq!(notice["newer"], false);
    let (_, health) = server
        .json(reqwest::Method::GET, "/v1/health", None, None)
        .await;
    assert_eq!(health["status"], "ok");
}
