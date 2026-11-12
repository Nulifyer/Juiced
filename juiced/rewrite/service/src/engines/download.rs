use crate::artifacts::{ArtifactError, SourcePolicy, checked_client, source_url};
use reqwest::{StatusCode, header};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

const MAX_REDIRECTS: usize = 5;
const RELEASE_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TRANSFER_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
const USER_AGENT: &str = "juiced-engine-installer";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadError {
    Offline,
    SourcePolicy,
    Network,
    Status,
    DigestUnavailable,
    HashMismatch,
    SizeMismatch,
    Storage,
    Deadline,
    Cancelled,
}

impl DownloadError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Offline => "engine_source_offline",
            Self::SourcePolicy => "engine_source_policy",
            Self::Network => "engine_network",
            Self::Status => "engine_source_status",
            Self::DigestUnavailable => "engine_digest_unavailable",
            Self::HashMismatch => "engine_hash_mismatch",
            Self::SizeMismatch => "engine_size_mismatch",
            Self::Storage => "engine_storage",
            Self::Deadline => "engine_download_deadline",
            Self::Cancelled => "cancelled",
        }
    }
}

impl From<ArtifactError> for DownloadError {
    fn from(error: ArtifactError) -> Self {
        match error {
            ArtifactError::Offline => Self::Offline,
            ArtifactError::SourcePolicy => Self::SourcePolicy,
            _ => Self::Network,
        }
    }
}

async fn open(
    url: &str,
    policy: SourcePolicy,
    accept: &'static str,
) -> Result<reqwest::Response, DownloadError> {
    let mut url = source_url(url, policy, false)?;
    for _ in 0..=MAX_REDIRECTS {
        let client = checked_client(&url, policy).await?;
        let request = client
            .get(url.clone())
            .header(header::ACCEPT, accept)
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::USER_AGENT, USER_AGENT)
            .send();
        let response = tokio::time::timeout(IDLE_TIMEOUT, request)
            .await
            .map_err(|_| DownloadError::Network)?
            .map_err(|_| DownloadError::Network)?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(DownloadError::SourcePolicy)?;
            let next = url
                .join(location)
                .map_err(|_| DownloadError::SourcePolicy)?;
            url = source_url(next.as_str(), policy, true)?;
            continue;
        }
        if response.status() != StatusCode::OK {
            return Err(DownloadError::Status);
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| value != "identity")
        {
            return Err(DownloadError::SourcePolicy);
        }
        return Ok(response);
    }
    Err(DownloadError::SourcePolicy)
}

pub async fn published_digest(
    release_url: &str,
    asset: &str,
    policy: SourcePolicy,
) -> Result<(String, u64), DownloadError> {
    let mut response = open(release_url, policy, "application/vnd.github+json").await?;
    if response
        .content_length()
        .is_some_and(|length| length > RELEASE_DOCUMENT_BYTES as u64)
    {
        return Err(DownloadError::DigestUnavailable);
    }
    let mut body = Vec::new();
    while let Some(chunk) = tokio::time::timeout(IDLE_TIMEOUT, response.chunk())
        .await
        .map_err(|_| DownloadError::Network)?
        .map_err(|_| DownloadError::Network)?
    {
        if body.len() + chunk.len() > RELEASE_DOCUMENT_BYTES {
            return Err(DownloadError::DigestUnavailable);
        }
        body.extend_from_slice(&chunk);
    }
    digest_from_release(&body, asset)
}

pub fn digest_from_release(body: &[u8], asset: &str) -> Result<(String, u64), DownloadError> {
    let document: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| DownloadError::DigestUnavailable)?;
    let entry = document["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|entry| entry["name"] == asset))
        .ok_or(DownloadError::DigestUnavailable)?;
    let digest = entry["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or(DownloadError::DigestUnavailable)?;
    let size = entry["size"]
        .as_u64()
        .filter(|size| *size > 0)
        .ok_or(DownloadError::DigestUnavailable)?;
    Ok((digest.to_owned(), size))
}

/// Size and SHA-256 are confirmed only at end of stream, so callers must not extract the
/// written file after an error.
pub async fn fetch(
    url: &str,
    policy: SourcePolicy,
    expected_sha256: &str,
    expected_bytes: u64,
    target: &Path,
    progress: &AtomicU64,
    stopped: impl Fn() -> bool,
) -> Result<(), DownloadError> {
    let deadline = Instant::now() + TRANSFER_DEADLINE;
    let mut response = open(url, policy, "application/octet-stream").await?;
    if response
        .content_length()
        .is_some_and(|length| length != expected_bytes)
    {
        return Err(DownloadError::SizeMismatch);
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .await
        .map_err(|_| DownloadError::Storage)?;
    let mut digest = Sha256::new();
    let mut written = 0u64;
    loop {
        if stopped() {
            return Err(DownloadError::Cancelled);
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(DownloadError::Deadline)?;
        let chunk = tokio::time::timeout(IDLE_TIMEOUT.min(remaining), response.chunk())
            .await
            .map_err(|_| DownloadError::Network)?
            .map_err(|_| DownloadError::Network)?;
        let Some(chunk) = chunk else {
            break;
        };
        written = written
            .checked_add(chunk.len() as u64)
            .filter(|bytes| *bytes <= expected_bytes)
            .ok_or(DownloadError::SizeMismatch)?;
        digest.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|_| DownloadError::Storage)?;
        progress.store(written, Ordering::Release);
    }
    if written != expected_bytes {
        return Err(DownloadError::SizeMismatch);
    }
    if format!("{:x}", digest.finalize()) != expected_sha256 {
        return Err(DownloadError::HashMismatch);
    }
    file.sync_all().await.map_err(|_| DownloadError::Storage)?;
    Ok(())
}
