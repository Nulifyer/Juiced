mod catalog;
mod hub;
mod quant;
#[cfg(test)]
mod tests;

pub use hub::{HubLimits, Token};

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Task {
    Chat,
    Embeddings,
    Transcription,
    Tts,
    Image,
}

impl Task {
    fn pipeline_filter(self) -> Option<&'static str> {
        match self {
            Self::Chat | Self::Embeddings => None,
            Self::Transcription => Some("automatic-speech-recognition"),
            Self::Tts => Some("text-to-speech"),
            Self::Image => Some("text-to-image"),
        }
    }
    fn from_pipeline(tag: &str) -> Option<Self> {
        match tag {
            "text-generation" | "image-text-to-text" | "text2text-generation" => Some(Self::Chat),
            "feature-extraction" | "sentence-similarity" => Some(Self::Embeddings),
            "automatic-speech-recognition" => Some(Self::Transcription),
            "text-to-speech" | "text-to-audio" => Some(Self::Tts),
            "text-to-image" => Some(Self::Image),
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
}

impl PullRecord {
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
        }
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
                .take(limit)
                .collect()
        } else {
            Vec::new()
        };
        let blocked = self.hub.blocked();
        let skip_hub = query.hub == Some(false);
        let mut hub_error = blocked;
        let mut cached = None;
        let page = if blocked.is_none() && !skip_hub {
            match self
                .hub
                .search(&text, task, limit, query.cursor.as_deref())
                .await
            {
                Ok((page, hit)) => {
                    cached = Some(hit);
                    Some(page)
                }
                Err(error) => {
                    hub_error = Some(error);
                    None
                }
            }
        } else {
            None
        };
        let catalog_repos: BTreeSet<&str> = catalog_matches
            .iter()
            .map(|model| model.repo.as_str())
            .collect();
        let hub_repos: Vec<&RepoSummary> = page
            .as_ref()
            .map(|page| {
                page.repos
                    .iter()
                    .filter(|repo| !catalog_repos.contains(repo.id.as_str()))
                    .filter(|repo| {
                        task.is_none_or(|task| task.accepts(repo.pipeline_tag.as_deref()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut wanted: Vec<String> = catalog_repos
            .iter()
            .map(|repo| (*repo).to_owned())
            .collect();
        wanted.extend(hub_repos.iter().map(|repo| repo.id.clone()));
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
        let summaries: BTreeMap<&str, &RepoSummary> = page
            .as_ref()
            .map(|page| {
                page.repos
                    .iter()
                    .map(|repo| (repo.id.as_str(), repo))
                    .collect()
            })
            .unwrap_or_default();
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
        for repo in hub_repos {
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
            if entry.variants.as_ref().is_some_and(Vec::is_empty) {
                continue;
            }
            entries.push(entry);
        }
        if first_page && let Some(store) = &store {
            attach_local(&mut entries, self.local_files(store), &terms, task, limit);
        }
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
            .map(|entry| entry_json(entry, presence.as_ref(), pullable))
            .collect();
        let hub_status = match hub_error {
            None if skip_hub => "skipped",
            None => "ok",
            Some(HubError::Offline) => "offline",
            Some(HubError::SourcePolicy | HubError::Unconfigured) => "blocked",
            Some(_) => "unavailable",
        };
        Ok(json!({
            "schema_version": 1,
            "query": text,
            "task": task,
            "offline": self.policy == SourcePolicy::Offline,
            "source_policy": self.policy,
            "artifact_generation": store.as_ref().map(|store| store.generation()),
            "sources": {
                "catalog": {"status": "ok", "count": catalog_matches.len()},
                "huggingface": {
                    "status": hub_status, "error": hub_error.map(HubError::code),
                    "cached": cached, "skipped": page.as_ref().map(|page| page.skipped),
                },
                "local": {"status": if store.is_some() { "ok" } else { "unavailable" }},
            },
            "data": data,
            "page": {
                "limit": limit,
                "next_cursor": page.as_ref().and_then(|page| page.next_cursor.clone()),
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
            pulls.remove(index);
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
        if let Some(bundle) = catalog::catalog()
            .models
            .iter()
            .filter(|model| model.repo == request.repo)
            .find_map(|model| model.bundle(&info.files))
        {
            variants.insert(0, bundle);
        }
        let mut selected: Vec<&quant::Variant> = Vec::new();
        for file in &requested {
            let variant = variants
                .iter()
                .find(|variant| variant.files.iter().any(|known| &known.path == file))
                .ok_or_else(|| {
                    library_error(
                        StatusCode::NOT_FOUND,
                        "library_file_not_found",
                        "The selected model file is not in this repository revision.",
                    )
                })?;
            if !selected
                .iter()
                .any(|known| known.primary == variant.primary)
            {
                selected.push(variant);
            }
        }
        let [variant] = selected.as_slice() else {
            return Err(invalid(
                "Select files from exactly one quantization variant.",
            ));
        };
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
        let key = format!("{}@{revision}:{}", request.repo, variant.primary);
        if let Some(existing) = self.find_pull(&key) {
            let mut view = existing
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .view();
            view["reused_pull"] = json!(true);
            return Ok(view);
        }
        let names = member_names(&variant.files);
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
            let specification = ArtifactSetSpec {
                schema_version: 1,
                origin: ArtifactSetOrigin {
                    importer_id: IMPORTER_ID.into(),
                    importer_version: IMPORTER_VERSION.into(),
                    source_revision: revision.clone(),
                },
                primary: names[0].clone(),
                members: parts
                    .iter()
                    .enumerate()
                    .map(|(index, part)| ArtifactSetMember {
                        name: names[index].clone(),
                        role: variant.roles.get(index).copied().unwrap_or(if index == 0 {
                            ArtifactRole::Model
                        } else {
                            ArtifactRole::WeightShard
                        }),
                        artifact: part.request.expected_sha256.clone(),
                        bytes: part.request.maximum_bytes,
                    })
                    .collect(),
            };
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
                Err(ArtifactError::Limit) if store.active_work_full() => {}
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
        }));
        let view = record
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .view();
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
                        Ok(status) => Some(status),
                        Err(ArtifactError::Conflict) => None,
                        Err(ArtifactError::Limit) if store.active_work_full() => None,
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
                for (index, status) in updates {
                    record.parts[index].status = status;
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
                return;
            }
            tokio::time::sleep(self.poll).await;
        }
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
        for _ in 0..16 {
            match store.publish_set(specification.clone(), store.generation(), &operation) {
                Ok(_) => return SetState::Published,
                Err(ArtifactError::Conflict) => continue,
                Err(error) => return SetState::Failed(error.code()),
            }
        }
        SetState::Failed(ArtifactError::Conflict.code())
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
            entry.variants = Some(grouped(&files, model));
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
            variants: Some(quant::group(&metas)),
        });
    }
}

fn entry_json(entry: &Entry, presence: Option<&Presence>, pullable: bool) -> Value {
    let summary = entry.summary.as_ref();
    let variants = entry.variants.as_ref().map(|variants| {
        variants
            .iter()
            .map(|variant| variant_json(entry, variant, presence, pullable))
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
    pullable: bool,
) -> Value {
    let files: Vec<Value> = variant
        .files
        .iter()
        .map(|file| json!({"file": file.path, "bytes": file.bytes, "sha256": file.sha256}))
        .collect();
    let digests: Vec<&str> = variant
        .files
        .iter()
        .filter_map(|file| file.sha256.as_deref())
        .collect();
    let local = presence.map(|presence| {
        let present = digests
            .iter()
            .filter(|digest| presence.artifacts.contains_key(**digest))
            .count();
        let set = digests.first().and_then(|primary| {
            presence
                .sets
                .get(*primary)?
                .iter()
                .find(|_| variant.files.len() > 1)
                .cloned()
        });
        json!({
            "present": variant.verifiable() && present == variant.files.len(),
            "present_files": present,
            "set": set,
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
    json!({
        "label": variant.label,
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
