use super::{
    ArtifactError, ArtifactStore, DownloadRequest, JobView, SourcePolicy, intake::UNLEARNED_DIGEST,
    source_url,
};
use reqwest::{Url, header};
use serde::Deserialize;
use std::sync::{Arc, atomic::Ordering};

/// A direct link where only the URL is required. Without a checksum the store keeps the
/// digest it computes and the file reads as unverified; without a size the server's
/// `Content-Length` is used, or the free space above the reserve caps the stream.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlDownload {
    pub source_url: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub expected_bytes: Option<u64>,
    #[serde(default)]
    pub maximum_bytes: Option<u64>,
}

impl ArtifactStore {
    pub async fn start_url_download(
        self: &Arc<Self>,
        request: UrlDownload,
        expected_generation: u64,
    ) -> Result<JobView, ArtifactError> {
        if self.policy == SourcePolicy::Offline {
            return Err(ArtifactError::Offline);
        }
        let url = source_url(&request.source_url, self.policy, false)?;
        let sha256 = request
            .expected_sha256
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
        let expected = match request.expected_bytes {
            Some(bytes) => Some(bytes),
            None => self.probe_length(url.clone()).await?,
        };
        let maximum = match (request.maximum_bytes, expected) {
            (Some(maximum), _) => maximum,
            (None, Some(bytes)) => bytes,
            (None, None) => self.streaming_ceiling()?,
        };
        let computed = sha256.is_none();
        let download = DownloadRequest {
            source_url: url.to_string(),
            expected_sha256: sha256.unwrap_or_else(|| UNLEARNED_DIGEST.to_owned()),
            expected_bytes: expected,
            maximum_bytes: maximum,
        };
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            store.start_download_with(download, expected_generation, computed)
        })
        .await
        .map_err(|_| ArtifactError::Io(std::io::Error::other("Download start task failed")))?
    }

    /// The free space above the reserve, for a source that does not announce its length.
    fn streaming_ceiling(&self) -> Result<u64, ArtifactError> {
        let reserve = self.free_reserve.load(Ordering::Acquire);
        let root = self.lease.root();
        let Some(volume) = crate::disk::existing_ancestor(root).and_then(crate::disk::volume)
        else {
            return Ok(self.limits.maximum_artifact_bytes);
        };
        let ceiling = volume
            .available_bytes
            .saturating_sub(reserve)
            .min(self.limits.maximum_artifact_bytes);
        if ceiling == 0 {
            return Err(ArtifactError::InsufficientSpace(Box::new(
                crate::disk::Shortfall {
                    path: root.to_owned(),
                    required_bytes: 1,
                    available_bytes: volume.available_bytes,
                    reserve_bytes: reserve,
                    shortfall_bytes: reserve
                        .saturating_add(1)
                        .saturating_sub(volume.available_bytes),
                },
            )));
        }
        Ok(ceiling)
    }

    /// The file's length from `HEAD`, or from a one-byte range request when `HEAD` is
    /// refused or sends none; `None` when the server reports neither.
    async fn probe_length(&self, url: Url) -> Result<Option<u64>, ArtifactError> {
        if let Some(length) = self.probe(url.clone(), reqwest::Method::HEAD).await? {
            return Ok(Some(length));
        }
        self.probe(url, reqwest::Method::GET).await
    }

    async fn probe(
        &self,
        mut url: Url,
        method: reqwest::Method,
    ) -> Result<Option<u64>, ArtifactError> {
        for _ in 0..=self.limits.maximum_redirects {
            let client = self.client(&url).await?;
            let mut request = client
                .request(method.clone(), url.clone())
                .header(header::ACCEPT_ENCODING, "identity")
                .timeout(self.limits.request_timeout);
            if method == reqwest::Method::GET {
                request = request.header(header::RANGE, "bytes=0-0");
            }
            if let Some((prefix, token)) = self.credential.get()
                && url.as_str().starts_with(&format!("{prefix}/"))
            {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.map_err(|_| ArtifactError::Network)?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(ArtifactError::SourcePolicy)?;
                let next = url
                    .join(location)
                    .map_err(|_| ArtifactError::SourcePolicy)?;
                url = source_url(next.as_str(), self.policy, true)?;
                continue;
            }
            let headers = response.headers();
            let number = |name| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            let length = match response.status() {
                reqwest::StatusCode::PARTIAL_CONTENT => number(header::CONTENT_RANGE)
                    .and_then(|range| range.rsplit_once('/').map(|(_, total)| total.to_owned()))
                    .and_then(|total| total.parse::<u64>().ok()),
                status if status.is_success() => {
                    number(header::CONTENT_LENGTH).and_then(|length| length.parse::<u64>().ok())
                }
                _ => None,
            };
            return Ok(length.filter(|length| *length > 0));
        }
        Err(ArtifactError::SourcePolicy)
    }
}
