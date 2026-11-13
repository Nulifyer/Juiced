mod catalog;
pub(crate) mod checkpoint;
mod durable;
mod hub;
mod local;
pub(crate) mod native;
mod quant;
#[cfg(test)]
mod tests;

pub use hub::{HubLimits, Token};
pub use local::{
    Hub as LocalHub, LocalFile as LocalInventoryFile, LocalInventory, Registered, ScanInputs,
    Storage as LocalStorage, remove_cache_file, writable,
};
pub const LOCAL_STORE_LISTING: usize = local::STORE_LISTING;

use crate::{
    ApiError,
    artifacts::{
        ArtifactError, ArtifactRole, ArtifactSetMember, ArtifactSetOrigin, ArtifactSetSpec,
        ArtifactStore, DownloadRequest, JobState, Presence, SetOperation, SourcePolicy,
    },
};
use axum::http::StatusCode;
use futures_util::StreamExt;
use hub::{Hub, HubError, RepoInfo, RepoSummary};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

const DEFAULT_LIMIT: usize = 10;
const QUERY_BYTES: usize = 128;
const QUERY_TERMS: usize = 8;
const MAXIMUM_PULLS: usize = 64;
const MAXIMUM_PARTS: usize = 64;
const LOCAL_SOURCES: usize = 1024;
const IMPORTER_ID: &str = "huggingface";
const IMPORTER_VERSION: &str = "juiced-library-1";
const PUBLISH_DEADLINE: Duration = Duration::from_secs(3600);
const LOCAL_TTL: Duration = Duration::from_secs(30);

struct CachedLocal {
    key: u64,
    at: Instant,
    inventory: Arc<LocalInventory>,
}

/// Cache-only hits for search: repository-relative paths and blob digests of adopted weights.
#[derive(Default)]
struct CachePresence {
    paths: BTreeMap<(String, String), (std::path::PathBuf, bool)>,
    digests: BTreeMap<String, (std::path::PathBuf, bool)>,
}

impl CachePresence {
    fn find(
        &self,
        repo: &str,
        path: &str,
        digest: Option<&str>,
    ) -> Option<&(std::path::PathBuf, bool)> {
        self.paths
            .get(&(repo.to_owned(), path.to_owned()))
            .or_else(|| self.digests.get(digest?))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Task {
    Chat,
    Embeddings,
    Transcription,
    Tts,
    Image,
    #[serde(alias = "audio_generation")]
    AudioGeneration,
    #[serde(rename = "3d")]
    Model3d,
    Classify,
}

impl Task {
    fn pipeline_filter(self) -> Option<&'static str> {
        match self {
            Self::Chat | Self::Embeddings => None,
            Self::Transcription => Some("automatic-speech-recognition"),
            Self::Tts => Some("text-to-speech"),
            Self::Image => Some("text-to-image"),
            Self::AudioGeneration => Some("text-to-audio"),
            Self::Model3d => Some("image-to-3d"),
            Self::Classify => Some("text-classification"),
        }
    }
    fn from_pipeline(tag: &str) -> Option<Self> {
        match tag {
            "text-generation" | "image-text-to-text" | "text2text-generation" => Some(Self::Chat),
            "feature-extraction" | "sentence-similarity" => Some(Self::Embeddings),
            "automatic-speech-recognition" => Some(Self::Transcription),
            "text-to-speech" => Some(Self::Tts),
            "text-to-audio" => Some(Self::AudioGeneration),
            "text-to-image" => Some(Self::Image),
            "image-to-3d" => Some(Self::Model3d),
            "text-classification" => Some(Self::Classify),
            _ => None,
        }
    }
    fn accepts(self, pipeline: Option<&str>) -> bool {
        match pipeline.and_then(Self::from_pipeline) {
            Some(task) => task == self,
            None => self == Self::Chat && pipeline.is_none(),
        }
    }
}

fn name_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(crate) fn valid_repo(value: &str) -> bool {
    value
        .split_once('/')
        .is_some_and(|(owner, name)| name_part(owner) && name_part(name))
}

pub(crate) fn valid_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn valid_file(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
        })
}

fn library_error(status: StatusCode, code: &'static str, message: &str) -> ApiError {
    ApiError::new(status, code, message)
}

fn hub_api_error(error: HubError) -> ApiError {
    match error {
        HubError::Offline => ArtifactError::Offline.into(),
        HubError::SourcePolicy => ArtifactError::SourcePolicy.into(),
        HubError::Unconfigured => library_error(
            StatusCode::SERVICE_UNAVAILABLE,
            error.code(),
            "The model hub address is not configured correctly.",
        ),
        HubError::NotFound => library_error(
            StatusCode::NOT_FOUND,
            "library_repo_not_found",
            "The model repository or revision is not available from the hub.",
        ),
        HubError::Timeout => library_error(
            StatusCode::GATEWAY_TIMEOUT,
            error.code(),
            "The model hub did not answer in time.",
        ),
        _ => library_error(
            StatusCode::BAD_GATEWAY,
            error.code(),
            "The model hub response could not be used.",
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub hub: Option<bool>,
    pub task: Option<Task>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    /// Narrows results to what one engine runs; vllm, onnxruntime and thenoise search
    /// original checkpoints, any other engine searches GGUF files.
    #[serde(default)]
    pub engine: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub repo: String,
    pub file: Option<String>,
    pub files: Option<Vec<String>>,
    pub revision: Option<String>,
    pub expected_generation: u64,
}

struct Entry {
    id: String,
    repo: String,
    name: String,
    family: Option<String>,
    task: Option<Task>,
    source: &'static str,
    catalog: Option<&'static catalog::CatalogModel>,
    summary: Option<RepoSummary>,
    revision: Option<String>,
    metadata: &'static str,
    metadata_error: Option<&'static str>,
    files_truncated: Option<bool>,
    variants: Option<Vec<quant::Variant>>,
}

struct LocalFile {
    repo: String,
    revision: String,
    file: quant::FileMeta,
}

#[derive(Clone, Debug)]
enum PartStatus {
    Present {
        job: Option<String>,
    },
    Queued,
    Job {
        id: String,
        state: JobState,
        downloaded_bytes: u64,
        reused: bool,
    },
    Failed(&'static str),
}

#[derive(Clone, Debug)]
struct Part {
    file: String,
    name: Option<String>,
    request: DownloadRequest,
    status: PartStatus,
}

#[derive(Clone, Debug)]
enum SetState {
    Pending,
    Published,
    Failed(&'static str),
}

#[derive(Debug)]
struct PlannedSet {
    id: String,
    specification: ArtifactSetSpec,
    state: SetState,
}

#[derive(Debug)]
struct PullRecord {
    id: String,
    key: String,
    repo: String,
    revision: String,
    label: Option<String>,
    parts: Vec<Part>,
    set: Option<PlannedSet>,
    checkpoint: Option<checkpoint::Kind>,
}

impl PullRecord {
    /// A checkpoint is named after its repository and a catalog bundle by its native name;
    /// a bundle's first file (`ss_flow.gguf`) says nothing about the model.
    fn suggested_id(&self) -> Option<String> {
        if self.checkpoint.is_some() {
            return Some(suggested_id(&self.repo));
        }
        let primary = self.parts.first()?.file.as_str();
        catalog::catalog()
            .models
            .iter()
            .find(|model| {
                model.repo == self.repo
                    && !model.companions.is_empty()
                    && model.recommended_file.as_deref() == Some(primary)
            })
            .map(|model| model.native_id.clone())
    }

    fn state(&self) -> (&'static str, Option<&'static str>) {
        if let Some(code) = self.parts.iter().find_map(|part| match part.status {
            PartStatus::Failed(code) => Some(code),
            _ => None,
        }) {
            return ("failed", Some(code));
        }
        if self
            .parts
            .iter()
            .all(|part| matches!(part.status, PartStatus::Present { .. }))
        {
            return match self.set.as_ref().map(|set| &set.state) {
                None | Some(SetState::Published) => ("complete", None),
                Some(SetState::Pending) => ("publishing", None),
                Some(SetState::Failed(code)) => ("failed", Some(code)),
            };
        }
        if self.parts.iter().any(|part| {
            matches!(
                part.status,
                PartStatus::Job {
                    state: JobState::Paused | JobState::Interrupted,
                    ..
                }
            )
        }) {
            return ("waiting", None);
        }
        ("downloading", None)
    }
    fn terminal(&self) -> bool {
        matches!(self.state().0, "complete" | "failed")
    }
    fn view(&self) -> Value {
        let (state, error) = self.state();
        let files: Vec<Value> = self
            .parts
            .iter()
            .map(|part| {
                let (status, job, downloaded, reused, error) = match &part.status {
                    PartStatus::Present { job } => {
                        (json!("present"), job.clone(), None, false, None)
                    }
                    PartStatus::Queued => (json!("queued"), None, None, false, None),
                    PartStatus::Job {
                        id,
                        state,
                        downloaded_bytes,
                        reused,
                    } => (
                        serde_json::to_value(state).unwrap_or(Value::Null),
                        Some(id.clone()),
                        Some(*downloaded_bytes),
                        *reused,
                        None,
                    ),
                    PartStatus::Failed(code) => (json!("failed"), None, None, false, Some(*code)),
                };
                json!({
                    "file": part.file, "member": part.name, "bytes": part.request.expected_bytes,
                    "sha256": part.request.expected_sha256, "source_url": part.request.source_url,
                    "state": status, "job": job, "reused_job": reused,
                    "downloaded_bytes": downloaded, "error": error,
                })
            })
            .collect();
        let jobs: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|part| match &part.status {
                PartStatus::Job { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let (set_state, set_error) = match self.set.as_ref().map(|set| &set.state) {
            None => (None, None),
            Some(SetState::Pending) => (Some("pending"), None),
            Some(SetState::Published) => (Some("published"), None),
            Some(SetState::Failed(code)) => (Some("failed"), Some(*code)),
        };
        json!({
            "schema_version": 1, "pull": self.id, "state": state, "error": error,
            "repo": self.repo, "revision": self.revision, "label": self.label,
            "file": self.parts.first().map(|part| part.file.as_str()),
            "provenance": {
                "source": "huggingface", "repo": self.repo, "revision": self.revision,
                "files": self.parts.iter().map(|part| part.file.as_str()).collect::<Vec<_>>(),
            },
            "files": files, "jobs": jobs,
            "format": self.checkpoint,
            "engine": self.checkpoint.map(|kind| kind.engine().name()),
            "suggested_id": self.suggested_id(),
            "set": self.set.as_ref().map(|set| set.id.as_str()),
            "set_state": set_state, "set_error": set_error,
        })
    }
}

pub struct Library {
    hub: Hub,
    policy: SourcePolicy,
    poll: Duration,
    pulls: Mutex<Vec<Arc<Mutex<PullRecord>>>>,
    local_scan: tokio::sync::Mutex<()>,
    local: Mutex<Option<CachedLocal>>,
    journal: std::sync::OnceLock<Arc<crate::acquisitions::Journal>>,
    updates: Mutex<(u64, BTreeMap<String, bool>)>,
}

impl Library {
    pub fn from_environment(policy: SourcePolicy) -> Self {
        let base = std::env::var(hub::BASE_VARIABLE)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| hub::DEFAULT_BASE.to_owned());
        let token = std::env::var(hub::TOKEN_VARIABLE).ok().and_then(Token::new);
        Self::new(
            hub::parse_base(&base),
            token,
            policy,
            HubLimits::default(),
            Duration::from_millis(250),
        )
    }

    pub fn new(
        base: Option<reqwest::Url>,
        token: Option<Token>,
        policy: SourcePolicy,
        limits: HubLimits,
        poll: Duration,
    ) -> Self {
        Self {
            hub: Hub::new(base, token, policy, limits),
            policy,
            poll,
            pulls: Mutex::new(Vec::new()),
            local_scan: tokio::sync::Mutex::new(()),
            local: Mutex::new(None),
            journal: std::sync::OnceLock::new(),
            updates: Mutex::new((0, BTreeMap::new())),
        }
    }

    pub(crate) fn hub(&self) -> &Hub {
        &self.hub
    }

    /// Lets the artifact store send the hub token with file downloads from the hub, which
    /// gated repositories require.
    pub fn share_credential(&self, store: &ArtifactStore) {
        if let (Some(prefix), Some(token)) = (self.hub.base_prefix(), self.hub.token()) {
            store.set_origin_credential(prefix, token.to_owned());
        }
    }

    /// Whether the last explicit update check found newer upstream weights for this model;
    /// `None` until a check determined it.
    pub fn update_available(&self, model: &str) -> Option<bool> {
        self.updates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .1
            .get(model)
            .copied()
    }

    /// Changes whenever a check changes any determination, for list validators.
    pub fn updates_generation(&self) -> u64 {
        self.updates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
    }

    pub(crate) fn record_updates(&self, determined: BTreeMap<String, bool>) {
        let mut updates = self
            .updates
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut changed = false;
        for (model, available) in determined {
            changed |= updates.1.insert(model, available) != Some(available);
        }
        if changed {
            updates.0 += 1;
        }
    }

    fn cached_local(&self) -> Option<Arc<LocalInventory>> {
        self.local
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|cached| cached.inventory.clone())
    }

    pub fn invalidate_local(&self) {
        *self.local.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Returns the bounded on-disk inventory, rescanning (stat only) when `key` changed, the
    /// cached scan is older than the TTL, or `fresh` is set. Concurrent callers share one scan.
    pub async fn local_inventory(
        &self,
        key: u64,
        fresh: bool,
        inputs: impl FnOnce() -> ScanInputs + Send + 'static,
    ) -> Result<Arc<LocalInventory>, ApiError> {
        let current = |allow: bool| {
            self.local
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .filter(|cached| allow && cached.key == key && cached.at.elapsed() < LOCAL_TTL)
                .map(|cached| cached.inventory.clone())
        };
        if let Some(inventory) = current(!fresh) {
            return Ok(inventory);
        }
        let started = Instant::now();
        let _scan = self.local_scan.lock().await;
        if let Some(inventory) = self
            .local
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|cached| cached.key == key && cached.at >= started)
            .map(|cached| cached.inventory.clone())
        {
            return Ok(inventory);
        }
        if let Some(inventory) = current(!fresh) {
            return Ok(inventory);
        }
        let prefix = self.hub.base_prefix();
        let inventory = tokio::task::spawn_blocking(move || {
            let inputs = inputs();
            let parse = |url: &str| hub::parse_resolve(prefix.as_deref()?, url);
            local::scan(&inputs, &parse)
        })
        .await
        .map_err(|_| {
            library_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "local_inventory_interrupted",
                "Listing the files on this machine was interrupted. Retry.",
            )
        })?;
        let inventory = Arc::new(inventory);
        *self.local.lock().unwrap_or_else(|error| error.into_inner()) = Some(CachedLocal {
            key,
            at: Instant::now(),
            inventory: inventory.clone(),
        });
        Ok(inventory)
    }

    fn cache_presence(&self) -> Option<CachePresence> {
        let inventory = self.cached_local()?;
        let mut presence = CachePresence::default();
        for (repo, file) in inventory.cache_files() {
            let hit = (file.location.clone(), file.read_only);
            presence
                .paths
                .insert((repo.to_owned(), file.path.clone()), hit.clone());
            if let Some(digest) = &file.digest {
                presence.digests.insert(digest.clone(), hit);
            }
        }
        Some(presence)
    }

    fn cache_local_files(&self) -> Vec<LocalFile> {
        let Some(inventory) = self.cached_local() else {
            return Vec::new();
        };
        inventory
            .cache_files()
            .filter(|(_, file)| matches!(file.role, "model" | "shard"))
            .filter(|(_, file)| quant::is_listed_file(&file.path))
            .take(LOCAL_SOURCES)
            .map(|(repo, file)| LocalFile {
                repo: repo.to_owned(),
                revision: file.revision.clone().unwrap_or_default(),
                file: quant::FileMeta {
                    path: file.path.clone(),
                    bytes: Some(file.bytes),
                    sha256: file.digest.clone(),
                    blob: None,
                },
            })
            .collect()
    }

    pub fn pulls_view(&self) -> Value {
        let pulls = self.pulls.lock().unwrap_or_else(|error| error.into_inner());
        let data: Vec<Value> = pulls
            .iter()
            .rev()
            .map(|record| {
                record
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .view()
            })
            .collect();
        json!({"schema_version": 1, "data": data})
    }

    pub fn dismiss_pull(&self, id: &str) -> Result<Value, ApiError> {
        let mut pulls = self.pulls.lock().unwrap_or_else(|error| error.into_inner());
        let index = pulls
            .iter()
            .position(|record| record.lock().unwrap_or_else(|error| error.into_inner()).id == id)
            .ok_or_else(|| {
                library_error(
                    StatusCode::NOT_FOUND,
                    "library_pull_not_found",
                    "No library acquisition with this identifier is retained.",
                )
            })?;
        if !pulls[index]
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .terminal()
        {
            return Err(library_error(
                StatusCode::CONFLICT,
                "library_pull_active",
                "This download is still running. Cancel or wait for it before dismissing it.",
            ));
        }
        pulls.remove(index);
        drop(pulls);
        self.forget_saved(id);
        Ok(json!({"pull": id, "dismissed": true}))
    }

    async fn infos(&self, repos: Vec<String>) -> BTreeMap<String, Result<Arc<RepoInfo>, HubError>> {
        let deadline = tokio::time::Instant::now() + self.hub.limits.search_deadline;
        let mut fetches = futures_util::stream::iter(repos.clone())
            .map(|repo| async move {
                let result = self.hub.info(&repo, None, false).await;
                (repo, result.map(|(info, _)| info))
            })
            .buffer_unordered(self.hub.limits.concurrency.max(1));
        let mut infos = BTreeMap::new();
        while let Ok(Some((repo, result))) = tokio::time::timeout_at(deadline, fetches.next()).await
        {
            infos.insert(repo, result);
        }
        for repo in repos {
            infos.entry(repo).or_insert(Err(HubError::Timeout));
        }
        infos
    }

    fn local_files(&self, store: &ArtifactStore) -> Vec<LocalFile> {
        let Some(prefix) = self.hub.base_prefix() else {
            return Vec::new();
        };
        store
            .completed_sources(&format!("{prefix}/"), LOCAL_SOURCES)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|record| {
                let (repo, revision, path) = self.hub.parse_resolve_url(&record.source_url)?;
                quant::is_listed_file(&path).then_some(LocalFile {
                    repo,
                    revision,
                    file: quant::FileMeta {
                        path,
                        bytes: Some(record.bytes),
                        sha256: Some(record.sha256),
                        blob: None,
                    },
                })
            })
            .collect()
    }

    pub async fn search(
        &self,
        store: Option<Arc<ArtifactStore>>,
        query: SearchQuery,
    ) -> Result<Value, ApiError> {
        let invalid = |message: &str| {
            library_error(StatusCode::BAD_REQUEST, "invalid_library_query", message)
        };
        let text = query.q.as_deref().unwrap_or("").trim().to_owned();
        if text.len() > QUERY_BYTES || text.chars().any(char::is_control) {
            return Err(invalid("Use a search text of at most 128 bytes."));
        }
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=self.hub.limits.maximum_repos).contains(&limit) {
            return Err(invalid(&format!(
                "Request between 1 and {} repositories.",
                self.hub.limits.maximum_repos
            )));
        }
        if query
            .cursor
            .as_deref()
            .is_some_and(|cursor| !hub::valid_cursor(cursor))
        {
            return Err(invalid("Use the next_cursor value from a previous page."));
        }
        let engine = match query.engine.as_deref() {
            None => None,
            Some(name) => Some(crate::engines::catalog::Engine::parse(name).ok_or_else(|| {
                invalid("Use an engine name such as llama.cpp, vllm, onnxruntime or thenoise.")
            })?),
        };
        let wanted_kind = engine.and_then(checkpoint::Kind::for_engine);
        let task = query.task;
        let first_page = query.cursor.is_none();
        let terms: Vec<String> = text
            .split_whitespace()
            .take(QUERY_TERMS)
            .map(str::to_ascii_lowercase)
            .collect();
        let catalog_matches: Vec<&'static catalog::CatalogModel> = if first_page {
            catalog::catalog()
                .models
                .iter()
                .filter(|model| model.matches(&terms, task))
                .filter(|model| match engine {
                    None => true,
                    Some(engine) => model
                        .engine
                        .map_or(wanted_kind.is_none(), |known| known == engine),
                })
                .take(limit)
                .collect()
        } else {
            Vec::new()
        };
        let blocked = self.hub.blocked();
        let skip_hub = query.hub == Some(false);
        let mut hub_error = blocked;
        let mut cached = None;
        // Without an engine, page one also searches original checkpoints; later pages and the
        // next cursor follow the GGUF listing.
        let streams: Vec<(hub::Stream, usize)> = match wanted_kind {
            Some(kind) => vec![(hub::Stream::Checkpoint(Some(kind.hub_filter())), limit)],
            None if engine.is_some() || !first_page => vec![(hub::Stream::Gguf, limit)],
            None => vec![
                (hub::Stream::Gguf, limit),
                (
                    hub::Stream::Checkpoint((task == Some(Task::Classify)).then_some("onnx")),
                    limit.min(CHECKPOINT_SEARCH),
                ),
            ],
        };
        let mut pages = Vec::new();
        if blocked.is_none() && !skip_hub {
            let searched = futures_util::future::join_all(streams.iter().map(|(stream, size)| {
                let task = match stream {
                    hub::Stream::Checkpoint(_) if task != Some(Task::Classify) => None,
                    _ => task,
                };
                self.hub
                    .search(&text, task, *size, query.cursor.as_deref(), *stream)
            }))
            .await;
            for ((stream, _), result) in streams.iter().zip(searched) {
                match result {
                    Ok((page, hit)) => {
                        cached = Some(cached.unwrap_or(true) && hit);
                        pages.push((*stream, page));
                    }
                    Err(error) => {
                        hub_error = hub_error.or(Some(error));
                    }
                }
            }
            if !pages.is_empty() {
                hub_error = None;
            }
        }
        let catalog_repos: BTreeSet<&str> = catalog_matches
            .iter()
            .map(|model| model.repo.as_str())
            .collect();
        let mut hub_repos: Vec<(&RepoSummary, bool)> = Vec::new();
        for (stream, page) in &pages {
            let checkpoints = matches!(stream, hub::Stream::Checkpoint(_));
            for repo in &page.repos {
                if catalog_repos.contains(repo.id.as_str())
                    || hub_repos.iter().any(|(known, _)| known.id == repo.id)
                {
                    continue;
                }
                if checkpoints {
                    if repo.formats.is_some_and(|formats| {
                        formats.gguf || !(formats.safetensors || formats.onnx)
                    }) {
                        continue;
                    }
                } else if task.is_some_and(|task| !task.accepts(repo.pipeline_tag.as_deref())) {
                    continue;
                }
                hub_repos.push((repo, checkpoints));
            }
        }
        let mut wanted: Vec<String> = catalog_repos
            .iter()
            .map(|repo| (*repo).to_owned())
            .collect();
        wanted.extend(hub_repos.iter().map(|(repo, _)| repo.id.clone()));
        let infos = if skip_hub {
            wanted
                .into_iter()
                .filter_map(|repo| {
                    let info = self.hub.cached_info(&repo)?;
                    Some((repo, Ok(info)))
                })
                .collect()
        } else if blocked.is_none() {
            self.infos(wanted).await
        } else {
            BTreeMap::new()
        };
        let summaries: BTreeMap<&str, &RepoSummary> = pages
            .iter()
            .flat_map(|(_, page)| page.repos.iter())
            .map(|repo| (repo.id.as_str(), repo))
            .collect();
        let mut entries = Vec::new();
        for model in &catalog_matches {
            let info = infos.get(&model.repo);
            let mut entry = Entry {
                id: model.id.clone(),
                repo: model.repo.clone(),
                name: model.name.clone(),
                family: model.family.clone(),
                task: Some(model.task),
                source: "catalog",
                catalog: Some(model),
                summary: summaries
                    .get(model.repo.as_str())
                    .map(|summary| (*summary).clone()),
                revision: None,
                metadata: "unavailable",
                metadata_error: blocked.map(HubError::code),
                files_truncated: None,
                variants: None,
            };
            apply_info(&mut entry, info, |path| model.includes(path), Some(model));
            entries.push(entry);
        }
        let mut found = Vec::new();
        for (repo, checkpoints) in hub_repos {
            let mut entry = Entry {
                id: repo.id.clone(),
                repo: repo.id.clone(),
                name: repo
                    .id
                    .split_once('/')
                    .map_or(repo.id.as_str(), |(_, name)| name)
                    .to_owned(),
                family: None,
                task: repo.pipeline_tag.as_deref().and_then(Task::from_pipeline),
                source: "huggingface",
                catalog: None,
                summary: Some(repo.clone()),
                revision: None,
                metadata: "unavailable",
                metadata_error: None,
                files_truncated: None,
                variants: None,
            };
            apply_info(&mut entry, infos.get(&repo.id), |_| true, None);
            if let Some(variants) = &mut entry.variants {
                variants.retain(|variant| {
                    let kind = variant.checkpoint;
                    (!checkpoints || kind.is_some())
                        && wanted_kind.is_none_or(|wanted| kind == Some(wanted))
                        && kind.is_none_or(|kind| task.is_none_or(|task| kind_task(kind) == task))
                });
                if variants.is_empty() {
                    continue;
                }
            } else if checkpoints {
                continue;
            }
            found.push(entry);
        }
        found.sort_by_key(|entry| {
            std::cmp::Reverse(entry.summary.as_ref().and_then(|summary| summary.downloads))
        });
        entries.extend(found);
        let learned = store
            .as_ref()
            .map(|store| self.learned_digests(store))
            .unwrap_or_default();
        for entry in &mut entries {
            let (repo, revision) = (entry.repo.clone(), entry.revision.clone());
            for variant in entry.variants.iter_mut().flatten() {
                for file in &mut variant.files {
                    if file.sha256.is_none()
                        && let Some(revision) = &revision
                    {
                        file.sha256 = learned
                            .get(&(repo.clone(), revision.clone(), file.path.clone()))
                            .cloned();
                    }
                }
            }
        }
        if first_page && wanted_kind.is_none() {
            let mut local = store
                .as_ref()
                .map(|store| self.local_files(store))
                .unwrap_or_default();
            local.extend(self.cache_local_files());
            attach_local(&mut entries, local, &terms, task, limit);
        }
        let cache = self.cache_presence();
        let digests: BTreeSet<String> = entries
            .iter()
            .flat_map(|entry| entry.variants.iter().flatten())
            .flat_map(|variant| &variant.files)
            .filter_map(|file| file.sha256.clone())
            .collect();
        let presence = store
            .as_ref()
            .and_then(|store| store.presence(&digests).ok());
        let pullable = store.is_some() && blocked.is_none();
        let data: Vec<Value> = entries
            .iter()
            .map(|entry| {
                entry_json(
                    entry,
                    presence.as_ref(),
                    cache.as_ref(),
                    pullable,
                    store.as_deref(),
                )
            })
            .collect();
        let hub_status = match hub_error {
            None if skip_hub => "skipped",
            None => "ok",
            Some(HubError::Offline) => "offline",
            Some(HubError::SourcePolicy | HubError::Unconfigured) => "blocked",
            Some(_) => "unavailable",
        };
        let next_cursor = pages.first().and_then(|(_, page)| page.next_cursor.clone());
        Ok(json!({
            "schema_version": 1,
            "query": text,
            "task": task,
            "engine": engine.map(crate::engines::catalog::Engine::name),
            "offline": self.policy == SourcePolicy::Offline,
            "source_policy": self.policy,
            "artifact_generation": store.as_ref().map(|store| store.generation()),
            "sources": {
                "catalog": {"status": "ok", "count": catalog_matches.len()},
                "huggingface": {
                    "status": hub_status, "error": hub_error.map(HubError::code),
                    "cached": cached,
                    "skipped": pages.first().map(|(_, page)| page.skipped),
                    "authenticated": self.hub.token().is_some(),
                },
                "local": {"status": if store.is_some() { "ok" } else { "unavailable" }},
            },
            "data": data,
            "page": {
                "limit": limit,
                "next_cursor": next_cursor,
            },
            "contains_credentials": false,
        }))
    }

    fn find_pull(&self, key: &str) -> Option<Arc<Mutex<PullRecord>>> {
        self.pulls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|record| {
                let record = record.lock().unwrap_or_else(|error| error.into_inner());
                record.key == key && !record.terminal()
            })
            .cloned()
    }

    fn reserve_pull_slot(&self) -> Result<(), ApiError> {
        let mut pulls = self.pulls.lock().unwrap_or_else(|error| error.into_inner());
        if pulls.len() < MAXIMUM_PULLS {
            return Ok(());
        }
        if let Some(index) = pulls.iter().position(|record| {
            record
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .terminal()
        }) {
            let removed = pulls.remove(index);
            drop(pulls);
            let id = removed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .id
                .clone();
            self.forget_saved(&id);
            return Ok(());
        }
        Err(library_error(
            StatusCode::TOO_MANY_REQUESTS,
            "library_pull_limit",
            "Too many library acquisitions are in progress. Retry after one finishes.",
        ))
    }

    pub fn pull_view(&self, id: &str) -> Result<Value, ApiError> {
        self.pulls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find_map(|record| {
                let record = record.lock().unwrap_or_else(|error| error.into_inner());
                (record.id == id).then(|| record.view())
            })
            .ok_or_else(|| {
                library_error(
                    StatusCode::NOT_FOUND,
                    "library_pull_not_found",
                    "No library acquisition with this identifier is retained.",
                )
            })
    }

    pub async fn pull(
        self: &Arc<Self>,
        store: Arc<ArtifactStore>,
        request: PullRequest,
    ) -> Result<Value, ApiError> {
        let invalid =
            |message: &str| library_error(StatusCode::BAD_REQUEST, "invalid_library_pull", message);
        if !valid_repo(&request.repo) {
            return Err(invalid("Use a repository identifier like owner/name."));
        }
        let requested: Vec<String> = match (&request.file, &request.files) {
            (Some(file), None) => vec![file.clone()],
            (None, Some(files)) if !files.is_empty() && files.len() <= MAXIMUM_PARTS => {
                files.clone()
            }
            _ => return Err(invalid("Select one file or a bounded list of files.")),
        };
        if requested.iter().any(|file| !valid_file(file)) {
            return Err(invalid("Use repository-relative model file paths."));
        }
        if request
            .revision
            .as_deref()
            .is_some_and(|revision| !valid_revision(revision))
        {
            return Err(invalid("Use a full 40-character commit revision."));
        }
        if store.source_policy() == SourcePolicy::Offline {
            return Err(ArtifactError::Offline.into());
        }
        if let Some(error) = self.hub.blocked() {
            return Err(hub_api_error(error));
        }
        let (info, _) = self
            .hub
            .info(
                &request.repo,
                request.revision.as_deref(),
                request.revision.is_none(),
            )
            .await
            .map_err(hub_api_error)?;
        let revision = info.revision.clone().ok_or_else(|| {
            library_error(
                StatusCode::BAD_GATEWAY,
                "library_revision_unknown",
                "The hub did not report the commit for this repository.",
            )
        })?;
        let mut variants = quant::group(&info.files);
        let bundles: Vec<_> = catalog::catalog()
            .models
            .iter()
            .filter(|model| model.repo == request.repo)
            .filter_map(|model| model.bundle(&info.files))
            .collect();
        variants.splice(0..0, bundles);
        variants.extend(checkpoint::variants(
            &info.files,
            info.summary.pipeline_tag.as_deref(),
        ));
        let listed = |file: &String| {
            variants
                .iter()
                .any(|variant| variant.files.iter().any(|known| &known.path == file))
        };
        if !requested.iter().all(listed) {
            return Err(library_error(
                StatusCode::NOT_FOUND,
                "library_file_not_found",
                "The selected model file is not in this repository revision.",
            ));
        }
        // A file may sit in several variants (a checkpoint's config.json, a bundle's GGUF);
        // the variant named by its primary file wins, then the first that holds them all.
        let holding: Vec<&quant::Variant> = variants
            .iter()
            .filter(|variant| {
                requested
                    .iter()
                    .all(|file| variant.files.iter().any(|known| &known.path == file))
            })
            .collect();
        let variant = holding
            .iter()
            .find(|variant| requested.contains(&variant.primary))
            .or(holding.first())
            .copied()
            .ok_or_else(|| invalid("Select files from exactly one quantization variant."))?;
        if !variant.complete {
            return Err(library_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "library_split_incomplete",
                "The hub does not list every part of this split model.",
            ));
        }
        if !variant.verifiable() {
            return Err(library_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "library_file_unverifiable",
                "The hub did not publish a SHA-256 digest and size for every file.",
            ));
        }
        let mut variant = variant.clone();
        self.resolve_small_files(&store, &request.repo, &revision, &mut variant)
            .await?;
        let variant = &variant;
        let key = format!("{}@{revision}:{}", request.repo, variant.primary);
        if let Some(existing) = self.find_pull(&key) {
            let mut view = existing
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .view();
            view["reused_pull"] = json!(true);
            return Ok(view);
        }
        let names = checkpoint::member_names(variant)
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| member_names(&variant.files));
        let mut parts = Vec::with_capacity(variant.files.len());
        for (file, name) in variant.files.iter().zip(&names) {
            let bytes = file.bytes.expect("verifiable size");
            parts.push(Part {
                file: file.path.clone(),
                name: (variant.files.len() > 1).then(|| name.clone()),
                request: DownloadRequest {
                    source_url: self
                        .hub
                        .resolve_url(&request.repo, &revision, &file.path)
                        .map_err(hub_api_error)?,
                    expected_sha256: file.sha256.clone().expect("verifiable digest"),
                    expected_bytes: Some(bytes),
                    maximum_bytes: bytes,
                },
                status: PartStatus::Queued,
            });
        }
        let set = if parts.len() > 1 {
            let specification = set_specification(variant, &names, &revision)
                .ok_or_else(|| invalid("Select files from exactly one quantization variant."))?;
            let (specification, id) = store.plan_set(specification).map_err(ApiError::from)?;
            Some(PlannedSet {
                id,
                specification,
                state: SetState::Pending,
            })
        } else {
            None
        };
        self.reserve_pull_slot()?;
        let digests: BTreeSet<String> = parts
            .iter()
            .map(|part| part.request.expected_sha256.clone())
            .collect();
        let presence = store.presence(&digests).map_err(ApiError::from)?;
        for part in &mut parts {
            part.status = existing_status(&presence, &part.request.expected_sha256);
        }
        let remaining: u64 = parts
            .iter()
            .map(|part| match &part.status {
                PartStatus::Queued => part.request.maximum_bytes,
                PartStatus::Job {
                    downloaded_bytes, ..
                } => part.request.maximum_bytes.saturating_sub(*downloaded_bytes),
                _ => 0,
            })
            .sum();
        if let Err(ArtifactError::InsufficientSpace(shortfall)) = store.require_space(remaining) {
            return Err(shortfall.into_error("download this model"));
        }
        let mut guarded = Some(request.expected_generation);
        for part in &mut parts {
            if !matches!(part.status, PartStatus::Queued) {
                continue;
            }
            let generation = guarded.take();
            match start(&store, part.request.clone(), generation).await {
                Ok(status) => part.status = status,
                Err(error) if generation.is_some() => return Err(error.into()),
                Err(ArtifactError::Conflict) => {}
                Err(error) => part.status = PartStatus::Failed(error.code()),
            }
        }
        let record = Arc::new(Mutex::new(PullRecord {
            id: crate::storage::generated_id().map_err(|_| {
                library_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "library_pull_identifier",
                    "A library acquisition identifier could not be generated.",
                )
            })?,
            key,
            repo: request.repo.clone(),
            revision,
            label: variant.label.clone(),
            parts,
            set,
            checkpoint: variant.checkpoint,
        }));
        let view = {
            let record = record.lock().unwrap_or_else(|error| error.into_inner());
            self.persist(&record);
            record.view()
        };
        self.pulls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(record.clone());
        let library = self.clone();
        tokio::spawn(async move { library.drive(store, record).await });
        Ok(view)
    }

    async fn drive(self: Arc<Self>, store: Arc<ArtifactStore>, record: Arc<Mutex<PullRecord>>) {
        loop {
            if store.stopping() {
                return;
            }
            let snapshot: Vec<(usize, PartStatus, DownloadRequest)> = {
                let record = record.lock().unwrap_or_else(|error| error.into_inner());
                if record.terminal() {
                    return;
                }
                record
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(index, part)| (index, part.status.clone(), part.request.clone()))
                    .collect()
            };
            let mut updates = Vec::new();
            for (index, status, download) in snapshot {
                let next = match status {
                    PartStatus::Queued => match start(&store, download, None).await {
                        Ok(PartStatus::Queued) | Err(ArtifactError::Conflict) => None,
                        Ok(status) => Some(status),
                        Err(error) => Some(PartStatus::Failed(error.code())),
                    },
                    PartStatus::Job { id, reused, .. } => {
                        observe(&store, &id, reused, &download.expected_sha256)
                    }
                    _ => None,
                };
                if let Some(next) = next {
                    updates.push((index, next));
                }
            }
            let publish = {
                let mut record = record.lock().unwrap_or_else(|error| error.into_inner());
                let changed = updates.iter().any(|(index, status)| {
                    !matches!(
                        (&record.parts[*index].status, status),
                        (PartStatus::Job { id: left, state: a, .. }, PartStatus::Job { id: right, state: b, .. })
                            if left == right && a == b
                    )
                });
                for (index, status) in updates {
                    record.parts[index].status = status;
                }
                if changed {
                    self.persist(&record);
                }
                (record.state().0 == "publishing")
                    .then(|| record.set.as_ref().map(|set| set.specification.clone()))
                    .flatten()
            };
            if let Some(specification) = publish {
                let state = publish_set(&store, specification).await;
                let mut record = record.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(set) = &mut record.set {
                    set.state = state;
                }
                self.persist(&record);
                return;
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

const CHECKPOINT_SEARCH: usize = 6;

/// The console task a checkpoint kind serves.
fn kind_task(kind: checkpoint::Kind) -> Task {
    match kind {
        checkpoint::Kind::Transformers => Task::Chat,
        checkpoint::Kind::OnnxClassifier => Task::Classify,
        checkpoint::Kind::TheNoiseDiffusion => Task::Image,
    }
}

/// The model id a checkpoint pull suggests: the repository name.
fn suggested_id(repo: &str) -> String {
    repo.split_once('/')
        .map_or(repo, |(_, name)| name)
        .to_owned()
}

/// The artifact set a multi-file variant publishes; `None` until every digest is known.
fn set_specification(
    variant: &quant::Variant,
    names: &[String],
    revision: &str,
) -> Option<ArtifactSetSpec> {
    if names.len() != variant.files.len() {
        return None;
    }
    Some(ArtifactSetSpec {
        schema_version: 1,
        origin: ArtifactSetOrigin {
            importer_id: IMPORTER_ID.into(),
            importer_version: IMPORTER_VERSION.into(),
            source_revision: revision.to_owned(),
        },
        primary: names.first()?.clone(),
        members: variant
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                Some(ArtifactSetMember {
                    name: names[index].clone(),
                    role: variant.roles.get(index).copied().unwrap_or(if index == 0 {
                        ArtifactRole::Model
                    } else {
                        ArtifactRole::WeightShard
                    }),
                    artifact: file.sha256.clone()?,
                    bytes: file.bytes?,
                })
            })
            .collect::<Option<Vec<_>>>()?,
    })
}

impl Library {
    /// SHA-256 digests the store learned for hub files it downloaded, keyed by repository,
    /// revision and path, so files the hub publishes no digest for are recognised later.
    fn learned_digests(&self, store: &ArtifactStore) -> BTreeMap<(String, String, String), String> {
        let Some(prefix) = self.hub.base_prefix() else {
            return BTreeMap::new();
        };
        store
            .completed_sources(&format!("{prefix}/"), LOCAL_SOURCES)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|record| {
                let (repo, revision, path) = self.hub.parse_resolve_url(&record.source_url)?;
                Some(((repo, revision, path), record.sha256))
            })
            .collect()
    }

    /// The model id the pull that published `set` suggests.
    pub fn set_suggested_id(&self, set: &str) -> Option<String> {
        self.pulls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find_map(|record| {
                let record = record.lock().unwrap_or_else(|error| error.into_inner());
                (record.set.as_ref().is_some_and(|planned| planned.id == set))
                    .then(|| record.suggested_id())
                    .flatten()
            })
    }

    /// The repository name a downloaded checkpoint's primary file came from, as a model id.
    pub fn checkpoint_id(&self, store: &ArtifactStore, artifact: &str) -> Option<String> {
        self.learned_digests(store)
            .into_iter()
            .find(|(_, digest)| digest == artifact)
            .map(|((repo, _, _), _)| suggested_id(&repo))
    }

    /// Learns the SHA-256 of each small file the hub keeps outside LFS by fetching it once and
    /// checking its size and git blob id against the repository listing.
    async fn resolve_small_files(
        &self,
        store: &ArtifactStore,
        repo: &str,
        revision: &str,
        variant: &mut quant::Variant,
    ) -> Result<(), ApiError> {
        if variant.files.iter().all(|file| file.sha256.is_some()) {
            return Ok(());
        }
        let learned = self.learned_digests(store);
        for file in &mut variant.files {
            if file.sha256.is_some() {
                continue;
            }
            if let Some(known) =
                learned.get(&(repo.to_owned(), revision.to_owned(), file.path.clone()))
            {
                file.sha256 = Some(known.clone());
                continue;
            }
            let (Some(blob), Some(bytes)) = (file.blob.as_deref(), file.bytes) else {
                return Err(library_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "library_file_unverifiable",
                    "The hub did not publish a SHA-256 digest and size for every file.",
                ));
            };
            let body = self
                .hub
                .fetch_file(repo, revision, &file.path, checkpoint::SMALL_FILE_BYTES)
                .await
                .map_err(hub_api_error)?;
            if body.len() as u64 != bytes || checkpoint::git_blob(&body) != blob {
                return Err(library_error(
                    StatusCode::BAD_GATEWAY,
                    "library_file_mismatch",
                    "A repository file did not match the size and git blob id the hub listed.",
                ));
            }
            file.sha256 = Some(format!(
                "{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(&body)
            ));
        }
        Ok(())
    }
}

fn existing_status(presence: &Presence, digest: &str) -> PartStatus {
    if presence.artifacts.contains_key(digest) {
        return PartStatus::Present { job: None };
    }
    presence
        .jobs
        .get(digest)
        .and_then(|jobs| {
            jobs.iter().find(|job| {
                !matches!(
                    job.state,
                    JobState::Complete
                        | JobState::Failed
                        | JobState::Cancelled
                        | JobState::Quarantined
                )
            })
        })
        .map_or(PartStatus::Queued, |job| PartStatus::Job {
            id: job.id.clone(),
            state: job.state,
            downloaded_bytes: job.downloaded_bytes,
            reused: true,
        })
}

async fn start(
    store: &Arc<ArtifactStore>,
    download: DownloadRequest,
    expected_generation: Option<u64>,
) -> Result<PartStatus, ArtifactError> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || {
        let mut attempts = if expected_generation.is_some() { 1 } else { 4 };
        // A finishing transfer frees its slot just after the store refuses a new one, so a
        // refusal with free slots is retried before it counts as a hard limit; a refusal
        // while every slot is busy leaves the part queued for the next pass.
        let mut slot_attempts = 8;
        loop {
            let generation = expected_generation.unwrap_or_else(|| store.generation());
            match store.start_download(download.clone(), generation) {
                Ok(job) => {
                    return Ok(PartStatus::Job {
                        id: job.id,
                        state: job.state,
                        downloaded_bytes: job.downloaded_bytes,
                        reused: false,
                    });
                }
                Err(ArtifactError::Conflict) if attempts > 1 => attempts -= 1,
                // Decided here, under the same refusal, so a slot freed between the refusal
                // and a later check cannot turn a busy store into a failed part.
                Err(ArtifactError::Limit) if store.active_work_full() => {
                    return Ok(PartStatus::Queued);
                }
                Err(ArtifactError::Limit) if slot_attempts > 1 => {
                    slot_attempts -= 1;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .unwrap_or(Err(ArtifactError::Stop))
}

fn observe(store: &ArtifactStore, id: &str, reused: bool, digest: &str) -> Option<PartStatus> {
    match store.job(id) {
        Ok(job) => Some(match job.state {
            JobState::Complete => PartStatus::Present {
                job: Some(id.to_owned()),
            },
            JobState::Failed => PartStatus::Failed("artifact_job_failed"),
            JobState::Cancelled => PartStatus::Failed("artifact_job_cancelled"),
            JobState::Quarantined => PartStatus::Failed("artifact_job_quarantined"),
            state => PartStatus::Job {
                id: id.to_owned(),
                state,
                downloaded_bytes: job.downloaded_bytes,
                reused,
            },
        }),
        Err(ArtifactError::NotFound) => {
            let digests = BTreeSet::from([digest.to_owned()]);
            Some(
                if store
                    .presence(&digests)
                    .is_ok_and(|presence| presence.artifacts.contains_key(digest))
                {
                    PartStatus::Present { job: None }
                } else {
                    PartStatus::Failed("artifact_job_missing")
                },
            )
        }
        Err(_) => None,
    }
}

async fn publish_set(store: &Arc<ArtifactStore>, specification: ArtifactSetSpec) -> SetState {
    let store = store.clone();
    tokio::task::spawn_blocking(move || {
        let operation = SetOperation::new(
            Instant::now() + PUBLISH_DEADLINE,
            Arc::new(AtomicBool::new(false)),
        );
        let mut last = ArtifactError::Conflict;
        for _ in 0..64 {
            match store.publish_set(specification.clone(), store.generation(), &operation) {
                Ok(_) => return SetState::Published,
                Err(ArtifactError::Conflict) => last = ArtifactError::Conflict,
                // Member downloads release their transfer slots just after they finish.
                Err(ArtifactError::Limit) => {
                    last = ArtifactError::Limit;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return SetState::Failed(error.code()),
            }
        }
        SetState::Failed(last.code())
    })
    .await
    .unwrap_or(SetState::Failed("artifact_operation_interrupted"))
}

fn member_names(files: &[quant::FileMeta]) -> Vec<String> {
    let label = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    let mut names: Vec<String> = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate() {
        let base = quant::basename(&file.path);
        let name = if label(base) && !names.iter().any(|known| known == base) {
            base.to_owned()
        } else {
            format!("part-{:05}-of-{:05}.gguf", index + 1, files.len())
        };
        names.push(name);
    }
    names
}

fn grouped(
    files: &[quant::FileMeta],
    model: Option<&catalog::CatalogModel>,
) -> Vec<quant::Variant> {
    match model.and_then(|model| model.bundle(files)) {
        Some(bundle) => vec![bundle],
        None => quant::group(files),
    }
}

fn apply_info(
    entry: &mut Entry,
    info: Option<&Result<Arc<RepoInfo>, HubError>>,
    include: impl Fn(&str) -> bool,
    model: Option<&catalog::CatalogModel>,
) {
    match info {
        Some(Ok(info)) => {
            let files: Vec<quant::FileMeta> = info
                .files
                .iter()
                .filter(|file| include(&file.path))
                .cloned()
                .collect();
            entry.revision = info.revision.clone();
            entry.metadata = "huggingface";
            entry.metadata_error = None;
            entry.files_truncated = Some(info.files_truncated);
            let mut variants = grouped(&files, model);
            if model.is_none() {
                variants.extend(checkpoint::variants(
                    &files,
                    info.summary.pipeline_tag.as_deref(),
                ));
            }
            entry.variants = Some(variants);
            if entry.summary.is_none() {
                entry.summary = Some(info.summary.clone());
            } else if let Some(summary) = &mut entry.summary {
                summary.license = summary.license.take().or(info.summary.license.clone());
                summary.gated = summary.gated.or(info.summary.gated);
            }
        }
        Some(Err(error)) => entry.metadata_error = Some(error.code()),
        None => {}
    }
}

fn attach_local(
    entries: &mut Vec<Entry>,
    local: Vec<LocalFile>,
    terms: &[String],
    task: Option<Task>,
    limit: usize,
) {
    let mut leftovers: BTreeMap<String, Vec<LocalFile>> = BTreeMap::new();
    for file in local {
        let mut attached = false;
        for entry in entries.iter_mut().filter(|entry| entry.repo == file.repo) {
            if entry
                .catalog
                .is_some_and(|model| !model.includes(&file.file.path))
            {
                continue;
            }
            attached = true;
            if entry.metadata == "huggingface" {
                continue;
            }
            entry.metadata = "local";
            entry.revision.get_or_insert_with(|| file.revision.clone());
            let variants = entry.variants.get_or_insert_with(Vec::new);
            let mut files: Vec<quant::FileMeta> = variants
                .drain(..)
                .flat_map(|variant| variant.files)
                .collect();
            if !files.iter().any(|known| known.path == file.file.path) {
                files.push(file.file.clone());
            }
            *variants = grouped(&files, entry.catalog);
        }
        if !attached {
            leftovers.entry(file.repo.clone()).or_default().push(file);
        }
    }
    if task.is_some() {
        return;
    }
    for (repo, files) in leftovers.into_iter().take(limit) {
        let haystack = std::iter::once(repo.to_ascii_lowercase())
            .chain(files.iter().map(|file| file.file.path.to_ascii_lowercase()))
            .collect::<Vec<_>>()
            .join(" ");
        if !terms.iter().all(|term| haystack.contains(term.as_str())) {
            continue;
        }
        let metas: Vec<quant::FileMeta> = files.iter().map(|file| file.file.clone()).collect();
        let variants = quant::group(&metas);
        if variants.is_empty() {
            continue;
        }
        entries.push(Entry {
            id: repo.clone(),
            name: repo
                .split_once('/')
                .map_or(repo.as_str(), |(_, name)| name)
                .to_owned(),
            repo: repo.clone(),
            family: None,
            task: None,
            source: "local",
            catalog: None,
            summary: None,
            revision: files.first().map(|file| file.revision.clone()),
            metadata: "local",
            metadata_error: None,
            files_truncated: None,
            variants: Some(variants),
        });
    }
}

fn entry_json(
    entry: &Entry,
    presence: Option<&Presence>,
    cache: Option<&CachePresence>,
    pullable: bool,
    store: Option<&ArtifactStore>,
) -> Value {
    let summary = entry.summary.as_ref();
    let variants = entry.variants.as_ref().map(|variants| {
        variants
            .iter()
            .map(|variant| variant_json(entry, variant, presence, cache, pullable, store))
            .collect::<Vec<_>>()
    });
    json!({
        "id": entry.id,
        "repo": entry.repo,
        "name": entry.name,
        "publisher": entry.repo.split('/').next(),
        "family": entry.family,
        "task": entry.task,
        "source": entry.source,
        "catalog_id": entry.catalog.map(|model| model.id.as_str()),
        "native_id": entry.catalog.map(|model| model.native_id.as_str()),
        "engine": entry.catalog.and_then(|model| model.engine),
        "recommended_quant": entry.catalog.and_then(|model| model.recommended_quant.as_deref()),
        "recommended_file": entry.catalog.and_then(|model| model.recommended_file.as_deref()),
        "license": entry.catalog.and_then(|model| model.license.clone()).or_else(|| summary.and_then(|summary| summary.license.clone())),
        "downloads": summary.and_then(|summary| summary.downloads),
        "likes": summary.and_then(|summary| summary.likes),
        "gated": summary.and_then(|summary| summary.gated),
        "pipeline_tag": summary.and_then(|summary| summary.pipeline_tag.as_deref()),
        "revision": entry.revision,
        "metadata": entry.metadata,
        "metadata_error": entry.metadata_error,
        "files_truncated": entry.files_truncated,
        "quants": variants,
    })
}

fn variant_json(
    entry: &Entry,
    variant: &quant::Variant,
    presence: Option<&Presence>,
    cache: Option<&CachePresence>,
    pullable: bool,
    store: Option<&ArtifactStore>,
) -> Value {
    // Several checkpoints can share a primary file such as config.json, so a checkpoint is
    // present only as the exact set its pull publishes.
    let checkpoint_set = variant.checkpoint.and_then(|_| {
        let names = checkpoint::member_names(variant)?;
        let specification = set_specification(variant, names, entry.revision.as_deref()?)?;
        store?.plan_set(specification).ok().map(|(_, id)| id)
    });
    let files: Vec<Value> = variant
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            json!({
                "file": file.path, "bytes": file.bytes, "sha256": file.sha256,
                "member": variant.names.get(index),
            })
        })
        .collect();
    let digests: Vec<&str> = variant
        .files
        .iter()
        .filter_map(|file| file.sha256.as_deref())
        .collect();
    let cached: Vec<&(std::path::PathBuf, bool)> = cache
        .map(|cache| {
            variant
                .files
                .iter()
                .filter_map(|file| cache.find(&entry.repo, &file.path, file.sha256.as_deref()))
                .collect()
        })
        .unwrap_or_default();
    let in_cache =
        variant.complete && !variant.files.is_empty() && cached.len() == variant.files.len();
    let local = (presence.is_some() || cache.is_some()).then(|| {
        let present = presence.map_or(0, |presence| {
            digests
                .iter()
                .filter(|digest| presence.artifacts.contains_key(**digest))
                .count()
        });
        let set = presence.and_then(|presence| {
            digests.first().and_then(|primary| {
                presence
                    .sets
                    .get(*primary)?
                    .iter()
                    .find(|id| {
                        variant.files.len() > 1
                            && (variant.checkpoint.is_none()
                                || checkpoint_set.as_deref() == Some(id.as_str()))
                    })
                    .cloned()
            })
        });
        let stored = variant.verifiable()
            && present == variant.files.len()
            && (variant.checkpoint.is_none() || set.is_some());
        json!({
            "present": stored || in_cache,
            "present_files": present.max(cached.len()),
            "set": set,
            "storage": if stored { Some("artifact_store") } else if in_cache { Some("huggingface_cache") } else { None },
            "path": (!stored && in_cache && variant.files.len() == 1).then(|| cached[0].0.clone()),
            "read_only": (!stored && in_cache).then(|| cached.iter().all(|hit| hit.1)),
        })
    });
    let jobs = presence.map(|presence| {
        variant
            .files
            .iter()
            .filter_map(|file| Some((file, presence.jobs.get(file.sha256.as_deref()?)?)))
            .flat_map(|(file, jobs)| {
                jobs.iter().filter(|job| job.state != JobState::Complete).map(|job| {
                    json!({
                        "file": file.path, "id": job.id, "revision": job.revision,
                        "state": job.state, "downloaded_bytes": job.downloaded_bytes,
                        "expected_bytes": job.expected_bytes, "writer_active": job.writer_active,
                    })
                })
            })
            .collect::<Vec<_>>()
    });
    let kind = if variant.checkpoint.is_some() {
        "checkpoint"
    } else if variant.roles.is_empty() {
        "quant"
    } else {
        "bundle"
    };
    json!({
        "label": variant.label,
        "kind": kind,
        "format": variant.checkpoint,
        "engine": variant
            .checkpoint
            .map(|kind| kind.engine())
            .or(entry.catalog.and_then(|model| model.engine))
            .map(crate::engines::catalog::Engine::name),
        "file": variant.primary,
        "parts": variant.parts,
        "complete": variant.complete,
        "bytes": variant.bytes(),
        "sha256": (variant.files.len() == 1 || !variant.roles.is_empty()).then(|| variant.files[0].sha256.clone()).flatten(),
        "files": files,
        "recommended": entry.catalog.is_some_and(|model| model.recommends(variant.label.as_deref(), &variant.primary)),
        "pullable": pullable && entry.metadata == "huggingface" && variant.verifiable(),
        "local": local,
        "jobs": jobs,
    })
}
