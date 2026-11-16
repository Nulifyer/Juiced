//! The release notice: at most one anonymous request a day for the latest published release.

use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    cmp::Ordering as Order,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_URL: &str = "https://api.github.com/repos/Nulifyer/Juiced/releases/latest";
/// Points the check at a stub. Set, the first check runs at once instead of after startup.
pub const URL_VARIABLE: &str = "JUICED_TEST_RELEASES_URL";
const RELEASES_PAGE: &str = "https://github.com/Nulifyer/Juiced/releases";
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const FIRST_CHECK: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(10);
const BODY_LIMIT: usize = 512 * 1024;
const VERSION_CHARS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub version: String,
    pub url: String,
    pub checked_at_ms: u64,
}

pub struct Releases {
    enabled: AtomicBool,
    latest: Mutex<Option<Latest>>,
    last_attempt: Mutex<Option<Instant>>,
    wake: tokio::sync::Notify,
    url: String,
    immediate: bool,
}

impl Releases {
    pub fn new(enabled: bool) -> Self {
        let custom = std::env::var(URL_VARIABLE)
            .ok()
            .filter(|url| !url.is_empty());
        Self::with_url(enabled, custom.as_deref())
    }

    pub fn with_url(enabled: bool, custom: Option<&str>) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            latest: Mutex::new(None),
            last_attempt: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
            url: custom.unwrap_or(DEFAULT_URL).to_owned(),
            immediate: custom.is_some(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        self.wake.notify_one();
    }

    pub fn latest(&self) -> Option<Latest> {
        self.latest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn until_due(&self) -> Duration {
        self.last_attempt
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .map_or(Duration::ZERO, |at| INTERVAL.saturating_sub(at.elapsed()))
    }

    pub async fn run(&self, client: reqwest::Client) {
        if !self.immediate {
            tokio::time::sleep(FIRST_CHECK).await;
        }
        loop {
            if self.enabled() && self.until_due().is_zero() {
                self.check(&client).await;
            }
            let wait = if self.enabled() {
                self.until_due()
            } else {
                INTERVAL
            };
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Offline, rate-limited or malformed answers keep the previous result and say nothing.
    pub async fn check(&self, client: &reqwest::Client) {
        *self
            .last_attempt
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
        let Some(body) = fetch(client, &self.url).await else {
            return;
        };
        let Some((version, url)) = parse_latest(&body) else {
            return;
        };
        *self
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Latest {
            version,
            url,
            checked_at_ms: now_ms(),
        });
    }

    pub fn projection(&self, current: &str) -> Value {
        let enabled = self.enabled();
        let latest = enabled.then(|| self.latest()).flatten();
        json!({
            "check": enabled,
            "current": current,
            "latest": latest.as_ref().map(|latest| &latest.version),
            "url": latest.as_ref().map(|latest| &latest.url),
            "checked_at_ms": latest.as_ref().map(|latest| latest.checked_at_ms),
            "newer": latest.as_ref().is_some_and(|latest| newer(current, &latest.version)),
        })
    }
}

async fn fetch(client: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "juiced")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if body.len() + chunk.len() > BODY_LIMIT {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(u64::MAX as u128) as u64
        })
}

pub fn parse_latest(body: &[u8]) -> Option<(String, String)> {
    let value: Value = serde_json::from_slice(body).ok()?;
    if value["draft"] == true || value["prerelease"] == true {
        return None;
    }
    let tag = value["tag_name"].as_str()?;
    if tag.len() > VERSION_CHARS {
        return None;
    }
    parse_version(tag)?;
    let url = value["html_url"]
        .as_str()
        .filter(|url| {
            url.strip_prefix(RELEASES_PAGE)
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .unwrap_or(RELEASES_PAGE);
    Some((tag.to_owned(), url.to_owned()))
}

type Version<'a> = ([u64; 3], Vec<&'a str>);

fn parse_version(text: &str) -> Option<Version<'_>> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let text = text.split_once('+').map_or(text, |(version, _)| version);
    let (core, pre) = text
        .split_once('-')
        .map_or((text, None), |(core, pre)| (core, Some(pre)));
    let mut parts = core.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    let pre: Vec<&str> = pre.map_or_else(Vec::new, |pre| pre.split('.').collect());
    if pre.iter().any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return None;
    }
    Some((numbers, pre))
}

fn compare_pre(left: &[&str], right: &[&str]) -> Order {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Order::Equal,
        (true, false) => return Order::Greater,
        (false, true) => return Order::Less,
        _ => {}
    }
    for (left, right) in left.iter().zip(right) {
        let order = match (left.parse::<u64>(), right.parse::<u64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Order::Less,
            (Err(_), Ok(_)) => Order::Greater,
            (Err(_), Err(_)) => left.cmp(right),
        };
        if order != Order::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

/// Semantic-version order. An unparsable running version never claims an update.
pub fn newer(current: &str, latest: &str) -> bool {
    let (Some((current, current_pre)), Some((latest, latest_pre))) =
        (parse_version(current), parse_version(latest))
    else {
        return false;
    };
    latest
        .cmp(&current)
        .then_with(|| compare_pre(&latest_pre, &current_pre))
        == Order::Greater
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicUsize};

    #[test]
    fn versions_follow_semantic_order() {
        assert!(newer("0.10.0", "v0.10.1"));
        assert!(newer("v0.10.0", "0.11.0"));
        assert!(newer("1.0.0-rc.27", "v1.0.0"));
        assert!(newer("1.0.0-rc.9", "1.0.0-rc.10"));
        assert!(!newer("0.10.1", "v0.10.1"));
        assert!(!newer("0.10.1", "v0.10.0"));
        assert!(!newer("1.0.0", "1.0.0-rc.30"));
        assert!(!newer("0.10.0+build.4", "0.10.0"));
        assert!(!newer("dev", "v9.9.9"));
        assert!(!newer("0.10.0", "latest"));
        assert!(!newer("0.10.0", "v1.2"));
    }

    #[test]
    fn latest_release_parsing_rejects_drafts_bad_tags_and_foreign_links() {
        let body = |value: Value| serde_json::to_vec(&value).unwrap();
        assert_eq!(
            parse_latest(&body(
                json!({"tag_name": "v0.10.1", "html_url": "https://github.com/Nulifyer/Juiced/releases/tag/v0.10.1"})
            )),
            Some((
                "v0.10.1".into(),
                "https://github.com/Nulifyer/Juiced/releases/tag/v0.10.1".into()
            ))
        );
        assert_eq!(
            parse_latest(&body(
                json!({"tag_name": "v0.10.1", "html_url": "https://example.com/x"})
            )),
            Some(("v0.10.1".into(), RELEASES_PAGE.into()))
        );
        assert_eq!(
            parse_latest(&body(
                json!({"tag_name": "v0.10.1", "html_url": "https://github.com/Nulifyer/Juiced/releases.evil/x"})
            )),
            Some(("v0.10.1".into(), RELEASES_PAGE.into()))
        );
        assert_eq!(
            parse_latest(&body(json!({"tag_name": "v0.10.1", "draft": true}))),
            None
        );
        assert_eq!(
            parse_latest(&body(json!({"tag_name": "v0.10.1", "prerelease": true}))),
            None
        );
        assert_eq!(parse_latest(&body(json!({"tag_name": "<script>"}))), None);
        assert_eq!(
            parse_latest(&body(
                json!({"tag_name": format!("v1.0.0-{}", "a".repeat(70))})
            )),
            None
        );
        assert_eq!(parse_latest(b"not json"), None);
    }

    async fn stub(status: u16, body: &'static str) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = vec![0; 4096];
                let read = socket.read(&mut request).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&request[..read]).to_lowercase();
                assert!(head.contains("user-agent: juiced\r\n"), "{head}");
                assert!(!head.contains("cookie") && !head.contains("authorization"));
                counted.fetch_add(1, Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (format!("http://{address}/latest"), hits)
    }

    #[tokio::test]
    async fn a_check_records_the_latest_release_and_failures_keep_the_previous_one() {
        let (url, hits) = stub(
            200,
            r#"{"tag_name":"v99.0.0","html_url":"https://github.com/Nulifyer/Juiced/releases/tag/v99.0.0"}"#,
        )
        .await;
        let releases = Releases::with_url(true, Some(&url));
        let client = reqwest::Client::new();
        releases.check(&client).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let shown = releases.projection("0.10.0");
        assert_eq!(shown["latest"], "v99.0.0");
        assert_eq!(shown["newer"], true);
        assert_eq!(shown["check"], true);
        assert!(shown["checked_at_ms"].as_u64().unwrap() > 0);

        let (failing, _) = stub(403, r#"{"message":"rate limited"}"#).await;
        let offline = Releases::with_url(true, Some(&failing));
        offline.check(&client).await;
        assert_eq!(offline.projection("0.10.0")["latest"], Value::Null);
        assert_eq!(offline.projection("0.10.0")["newer"], false);
        *offline.latest.lock().unwrap() = releases.latest();
        offline.check(&client).await;
        assert_eq!(offline.projection("0.10.0")["latest"], "v99.0.0");

        let unreachable = Releases::with_url(true, Some("http://127.0.0.1:9/latest"));
        unreachable.check(&client).await;
        assert_eq!(unreachable.projection("0.10.0")["latest"], Value::Null);
    }

    #[tokio::test]
    async fn the_loop_checks_once_a_day_and_never_while_off() {
        let (url, hits) = stub(200, r#"{"tag_name":"v0.10.0"}"#).await;
        let releases = Arc::new(Releases::with_url(false, Some(&url)));
        let running = releases.clone();
        let task = tokio::spawn(async move { running.run(reqwest::Client::new()).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        releases.set_enabled(true);
        for _ in 0..50 {
            if hits.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        releases.set_enabled(false);
        releases.set_enabled(true);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(releases.projection("0.10.0")["newer"], false);
        releases.set_enabled(false);
        assert_eq!(releases.projection("0.10.0")["latest"], Value::Null);
        task.abort();
    }
}
