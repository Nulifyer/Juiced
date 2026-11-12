use super::{Task, quant, valid_file, valid_repo, valid_revision};
use crate::artifacts::{ArtifactError, SourcePolicy};
use reqwest::{StatusCode, Url, header};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const DEFAULT_BASE: &str = "https://huggingface.co";
pub const BASE_VARIABLE: &str = "JUICED_HUGGINGFACE_URL";
pub const TOKEN_VARIABLE: &str = "HF_TOKEN";
const CURSOR_BYTES: usize = 1024;

#[derive(Clone, Debug)]
pub struct HubLimits {
    pub request_timeout: Duration,
    pub search_deadline: Duration,
    pub search_bytes: usize,
    pub info_bytes: usize,
    pub maximum_repos: usize,
    pub maximum_files: usize,
    pub ttl: Duration,
    pub cache_entries: usize,
    pub cache_bytes: usize,
    pub concurrency: usize,
}

impl Default for HubLimits {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            search_deadline: Duration::from_secs(20),
            search_bytes: 2 * 1024 * 1024,
            info_bytes: 8 * 1024 * 1024,
            maximum_repos: 20,
            maximum_files: 256,
            ttl: Duration::from_secs(120),
            cache_entries: 128,
            cache_bytes: 8 * 1024 * 1024,
            concurrency: 4,
        }
    }
}

pub struct Token(String);
impl std::fmt::Debug for Token {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Token(<redacted>)")
    }
}
impl Token {
    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty()
            && value.len() <= 512
            && value.bytes().all(|byte| byte.is_ascii_graphic()))
        .then_some(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HubError {
    Offline,
    Unconfigured,
    SourcePolicy,
    Unreachable,
    Timeout,
    NotFound,
    RateLimited,
    Status,
    TooLarge,
    Malformed,
}

impl HubError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Offline => "huggingface_offline",
            Self::Unconfigured => "huggingface_unconfigured",
            Self::SourcePolicy => "huggingface_source_policy",
            Self::Unreachable => "huggingface_unreachable",
            Self::Timeout => "huggingface_timeout",
            Self::NotFound => "huggingface_not_found",
            Self::RateLimited => "huggingface_rate_limited",
            Self::Status => "huggingface_status",
            Self::TooLarge => "huggingface_response_too_large",
            Self::Malformed => "huggingface_malformed",
        }
    }
    fn from_artifact(error: ArtifactError) -> Self {
        match error {
            ArtifactError::Offline => Self::Offline,
            ArtifactError::SourcePolicy => Self::SourcePolicy,
            _ => Self::Unreachable,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RepoSummary {
    pub id: String,
    pub downloads: Option<u64>,
    pub likes: Option<u64>,
    pub pipeline_tag: Option<String>,
    pub gated: Option<bool>,
    pub license: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SearchPage {
    pub repos: Vec<RepoSummary>,
    pub next_cursor: Option<String>,
    pub skipped: usize,
}

#[derive(Clone, Debug)]
pub struct RepoInfo {
    pub id: String,
    pub revision: Option<String>,
    pub files: Vec<quant::FileMeta>,
    pub files_truncated: bool,
    pub summary: RepoSummary,
}

pub struct TtlCache<V> {
    ttl: Duration,
    entries: usize,
    bytes: usize,
    state: Mutex<CacheState<V>>,
}
struct CacheState<V> {
    values: BTreeMap<String, (Instant, usize, Arc<V>)>,
    bytes: usize,
}
impl<V> TtlCache<V> {
    pub fn new(ttl: Duration, entries: usize, bytes: usize) -> Self {
        Self {
            ttl,
            entries,
            bytes,
            state: Mutex::new(CacheState {
                values: BTreeMap::new(),
                bytes: 0,
            }),
        }
    }
    pub fn get(&self, key: &str) -> Option<Arc<V>> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (stored, weight, value) = state.values.get(key)?;
        if stored.elapsed() < self.ttl {
            return Some(value.clone());
        }
        let weight = *weight;
        state.values.remove(key);
        state.bytes -= weight;
        None
    }
    pub fn insert(&self, key: String, value: Arc<V>, weight: usize) {
        let weight = weight.saturating_add(key.len());
        if self.entries == 0 || weight > self.bytes {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((_, previous, _)) = state.values.remove(&key) {
            state.bytes -= previous;
        }
        let ttl = self.ttl;
        let expired: Vec<String> = state
            .values
            .iter()
            .filter(|(_, (stored, _, _))| stored.elapsed() >= ttl)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            let (_, previous, _) = state.values.remove(&key).expect("expired entry");
            state.bytes -= previous;
        }
        while state.values.len() >= self.entries || state.bytes + weight > self.bytes {
            let oldest = state
                .values
                .iter()
                .min_by_key(|(_, (stored, _, _))| *stored)
                .map(|(key, _)| key.clone())
                .expect("bounded cache has an entry to evict");
            let (_, previous, _) = state.values.remove(&oldest).expect("oldest entry");
            state.bytes -= previous;
        }
        state.bytes += weight;
        state.values.insert(key, (Instant::now(), weight, value));
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values
            .len()
    }
}

pub struct Hub {
    base: Option<Url>,
    token: Option<Token>,
    policy: SourcePolicy,
    pub limits: HubLimits,
    searches: TtlCache<SearchPage>,
    infos: TtlCache<RepoInfo>,
}

pub fn parse_base(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim_end_matches('/')).ok()?;
    (matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/")
        .then_some(url)
}

impl Hub {
    pub fn new(
        base: Option<Url>,
        token: Option<Token>,
        policy: SourcePolicy,
        limits: HubLimits,
    ) -> Self {
        let searches = TtlCache::new(limits.ttl, limits.cache_entries, limits.cache_bytes / 4);
        let infos = TtlCache::new(limits.ttl, limits.cache_entries, limits.cache_bytes);
        Self {
            base,
            token,
            policy,
            limits,
            searches,
            infos,
        }
    }

    pub fn base_prefix(&self) -> Option<String> {
        self.base
            .as_ref()
            .map(|base| base.as_str().trim_end_matches('/').to_owned())
    }

    pub fn blocked(&self) -> Option<HubError> {
        if self.policy == SourcePolicy::Offline {
            return Some(HubError::Offline);
        }
        let Some(base) = &self.base else {
            return Some(HubError::Unconfigured);
        };
        crate::artifacts::policy_url(base.as_str(), self.policy)
            .err()
            .map(HubError::from_artifact)
    }

    fn url(&self, segments: &[&str]) -> Result<Url, HubError> {
        if let Some(error) = self.blocked() {
            return Err(error);
        }
        let mut url = self.base.clone().ok_or(HubError::Unconfigured)?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| HubError::Unconfigured)?;
            path.clear();
            for segment in segments {
                for part in segment.split('/') {
                    path.push(part);
                }
            }
        }
        Ok(url)
    }

    pub fn resolve_url(&self, repo: &str, revision: &str, file: &str) -> Result<String, HubError> {
        if !valid_repo(repo) || !valid_revision(revision) || !valid_file(file) {
            return Err(HubError::Malformed);
        }
        Ok(self.url(&[repo, "resolve", revision, file])?.to_string())
    }

    pub fn parse_resolve_url(&self, url: &str) -> Option<(String, String, String)> {
        let rest = url.strip_prefix(&self.base_prefix()?)?.strip_prefix('/')?;
        let mut parts = rest.splitn(5, '/');
        let owner = parts.next()?;
        let name = parts.next()?;
        if parts.next()? != "resolve" {
            return None;
        }
        let revision = parts.next()?;
        let file = parts.next()?;
        let repo = format!("{owner}/{name}");
        (valid_repo(&repo) && valid_revision(revision) && valid_file(file))
            .then(|| (repo, revision.to_owned(), file.to_owned()))
    }

    async fn fetch(&self, url: Url, maximum: usize) -> Result<(Vec<u8>, Option<String>), HubError> {
        let work = async {
            let client = crate::artifacts::policy_client(&url, self.policy)
                .await
                .map_err(HubError::from_artifact)?;
            let mut request = client
                .get(url.clone())
                .header(header::ACCEPT, "application/json")
                .header(header::ACCEPT_ENCODING, "identity")
                .header(header::USER_AGENT, "juiced-library/1");
            if let Some(token) = &self.token {
                request = request.bearer_auth(&token.0);
            }
            let mut response = request.send().await.map_err(|_| HubError::Unreachable)?;
            match response.status() {
                status if status.is_success() => {}
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                    return Err(HubError::NotFound);
                }
                StatusCode::TOO_MANY_REQUESTS => return Err(HubError::RateLimited),
                _ => return Err(HubError::Status),
            }
            if response
                .content_length()
                .is_some_and(|length| length > maximum as u64)
            {
                return Err(HubError::TooLarge);
            }
            let next = response
                .headers()
                .get(header::LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_cursor);
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| HubError::Unreachable)? {
                if body.len() + chunk.len() > maximum {
                    return Err(HubError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok((body, next))
        };
        tokio::time::timeout(self.limits.request_timeout, work)
            .await
            .map_err(|_| HubError::Timeout)?
    }

    pub async fn search(
        &self,
        query: &str,
        task: Option<Task>,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Arc<SearchPage>, bool), HubError> {
        let mut url = self.url(&["api", "models"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if !query.is_empty() {
                pairs.append_pair("search", query);
            }
            if task != Some(Task::Transcription) {
                pairs.append_pair("filter", "gguf");
            }
            if let Some(pipeline) = task.and_then(Task::pipeline_filter) {
                pairs.append_pair("pipeline_tag", pipeline);
            }
            pairs
                .append_pair("sort", "downloads")
                .append_pair("direction", "-1")
                .append_pair("limit", &limit.min(self.limits.maximum_repos).to_string());
            if let Some(cursor) = cursor {
                pairs.append_pair("cursor", cursor);
            }
        }
        let key = url.to_string();
        if let Some(page) = self.searches.get(&key) {
            return Ok((page, true));
        }
        let (body, next) = self.fetch(url, self.limits.search_bytes).await?;
        let page = parse_search(&body, self.limits.maximum_repos, next)?;
        let weight = body.len().min(self.limits.search_bytes);
        let page = Arc::new(page);
        self.searches.insert(key, page.clone(), weight);
        Ok((page, false))
    }

    pub async fn info(
        &self,
        repo: &str,
        revision: Option<&str>,
        fresh: bool,
    ) -> Result<(Arc<RepoInfo>, bool), HubError> {
        if !valid_repo(repo) || revision.is_some_and(|revision| !valid_revision(revision)) {
            return Err(HubError::Malformed);
        }
        let mut url = match revision {
            Some(revision) => self.url(&["api", "models", repo, "revision", revision])?,
            None => self.url(&["api", "models", repo])?,
        };
        url.query_pairs_mut().append_pair("blobs", "true");
        let key = url.to_string();
        if !fresh && let Some(info) = self.infos.get(&key) {
            return Ok((info, true));
        }
        let (body, _) = self.fetch(url, self.limits.info_bytes).await?;
        let info = parse_info(&body, self.limits.maximum_files)?;
        if !info.id.eq_ignore_ascii_case(repo) {
            return Err(HubError::Malformed);
        }
        if revision.is_some_and(|revision| info.revision.as_deref() != Some(revision)) {
            return Err(HubError::Malformed);
        }
        let weight = info
            .files
            .iter()
            .map(|file| file.path.len() + 96)
            .sum::<usize>()
            + 256;
        let info = Arc::new(info);
        self.infos.insert(key, info.clone(), weight);
        Ok((info, false))
    }
}

pub fn valid_cursor(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= CURSOR_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=_-".contains(&byte))
}

fn next_cursor(link: &str) -> Option<String> {
    link.split(',').find_map(|entry| {
        let (target, parameters) = entry.split_once(';')?;
        if !parameters
            .split(';')
            .any(|parameter| parameter.trim().eq_ignore_ascii_case("rel=\"next\""))
        {
            return None;
        }
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        let url = Url::parse(target).ok()?;
        let cursor = url
            .query_pairs()
            .find(|(name, _)| name == "cursor")
            .map(|(_, value)| value.into_owned())?;
        valid_cursor(&cursor).then_some(cursor)
    })
}

fn count(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn short_text(value: &Value, maximum: usize) -> Option<String> {
    value
        .as_str()
        .filter(|text| {
            !text.is_empty() && text.len() <= maximum && !text.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn gated(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(gated) => Some(*gated),
        Value::String(mode) if !mode.is_empty() => Some(true),
        _ => None,
    }
}

fn license(item: &Value) -> Option<String> {
    let card = &item["cardData"]["license"];
    short_text(card, 64)
        .or_else(|| {
            card.as_array()
                .and_then(|values| values.first())
                .and_then(|value| short_text(value, 64))
        })
        .or_else(|| {
            item["tags"].as_array()?.iter().find_map(|tag| {
                tag.as_str()?
                    .strip_prefix("license:")
                    .filter(|license| !license.is_empty() && license.len() <= 64)
                    .map(str::to_owned)
            })
        })
}

fn summary(item: &Value) -> Option<RepoSummary> {
    let id = item["id"].as_str().filter(|id| valid_repo(id))?;
    Some(RepoSummary {
        id: id.to_owned(),
        downloads: count(&item["downloads"]),
        likes: count(&item["likes"]),
        pipeline_tag: short_text(&item["pipeline_tag"], 64),
        gated: gated(&item["gated"]),
        license: license(item),
    })
}

pub fn parse_search(
    body: &[u8],
    maximum: usize,
    next_cursor: Option<String>,
) -> Result<SearchPage, HubError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| HubError::Malformed)?;
    let items = value.as_array().ok_or(HubError::Malformed)?;
    let mut page = SearchPage {
        repos: Vec::new(),
        next_cursor,
        skipped: 0,
    };
    for item in items.iter().take(maximum) {
        match summary(item) {
            Some(repo) if !page.repos.iter().any(|known| known.id == repo.id) => {
                page.repos.push(repo)
            }
            _ => page.skipped += 1,
        }
    }
    Ok(page)
}

fn digest(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

pub fn parse_info(body: &[u8], maximum_files: usize) -> Result<RepoInfo, HubError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| HubError::Malformed)?;
    let summary = summary(&value).ok_or(HubError::Malformed)?;
    let siblings = value["siblings"].as_array().ok_or(HubError::Malformed)?;
    let mut files = Vec::new();
    let mut files_truncated = false;
    for sibling in siblings {
        let Some(path) = sibling["rfilename"].as_str() else {
            continue;
        };
        if !valid_file(path) || !quant::is_listed_file(path) {
            continue;
        }
        if files.len() == maximum_files {
            files_truncated = true;
            break;
        }
        let lfs = &sibling["lfs"];
        let declared = count(&sibling["size"]);
        let lfs_bytes = count(&lfs["size"]);
        let consistent = declared.is_none() || lfs_bytes.is_none() || declared == lfs_bytes;
        files.push(quant::FileMeta {
            path: path.to_owned(),
            bytes: consistent
                .then_some(lfs_bytes.or(declared))
                .flatten()
                .filter(|bytes| *bytes > 0),
            sha256: consistent.then(|| digest(&lfs["sha256"])).flatten(),
        });
    }
    Ok(RepoInfo {
        id: summary.id.clone(),
        revision: value["sha"]
            .as_str()
            .filter(|revision| valid_revision(revision))
            .map(str::to_owned),
        files,
        files_truncated,
        summary,
    })
}
