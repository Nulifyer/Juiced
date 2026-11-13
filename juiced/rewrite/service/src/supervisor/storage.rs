//! The storage owner: one read view over every store Juiced writes or reads, and the only
//! path that reclaims space. Each reclaim reuses the rules of the store that owns the bytes.

use super::*;
use crate::{
    artifacts::{JobState, JobView, QuarantinedFileView},
    disk,
    library::{LocalInventory, LocalStorage},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

const WALK_ENTRIES: u64 = 400_000;
const WALK_DEADLINE: Duration = Duration::from_secs(8);
const VIEW_TTL: Duration = Duration::from_secs(15);
/// Measurements faster than this are repeated on every request instead of cached.
const SLOW_VIEW: Duration = Duration::from_millis(500);
const MAXIMUM_RECLAIM: usize = 64;
const PACKAGE_STORE: &str = "runtime-packages";

/// Directories v0.9.2 kept on its `/data` volume. They are recognised by path only, reported,
/// and removed only when an operator reclaims them.
const LEGACY: &[(&str, &str, bool)] = &[
    ("cache", "v0.9.2 engine builds and caches", true),
    ("prompt-state", "v0.9.2 prompt snapshots", true),
    (
        "config",
        "v0.9.2 settings, kept for rollback and settings import",
        false,
    ),
];

const ENGINE_DIRECTORIES: &[&str] = &[PACKAGE_STORE, "engine-staging", "bundled-engines"];
const CONFIG_ENTRIES: &[&str] = &[
    "config.json",
    crate::acquisitions::FILENAME,
    crate::native_store::descriptor::STORE_ROOT,
    crate::storage::CONTROL_FILE,
];

pub(super) fn library(
    config: &Config,
    root: Option<&crate::storage::DirectoryLease>,
    scope: Option<&Arc<crate::storage::LiveRoots>>,
    artifacts: Option<&Arc<crate::artifacts::ArtifactStore>>,
) -> Arc<crate::library::Library> {
    let library = Arc::new(crate::library::Library::from_environment(
        config.artifact_source_policy,
    ));
    if let (Some(root), Some(scope)) = (root, scope) {
        library.attach_journal(crate::acquisitions::Journal::open(
            root.clone(),
            scope.clone(),
        ));
    }
    if let Some(store) = artifacts {
        store.set_free_reserve(config.storage.reserve_bytes);
    }
    library
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reclaim {
    pub items: Vec<String>,
    #[serde(default)]
    pub preview: bool,
    #[serde(default)]
    pub expected_revision: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Candidate {
    id: String,
    kind: &'static str,
    title: String,
    path: Option<String>,
    bytes: u64,
    refusal: Option<&'static str>,
    message: Option<String>,
    references: Vec<String>,
    /// Owned Hugging Face cache files a model removal also deletes.
    files: Vec<String>,
}

impl Candidate {
    fn json(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "title": self.title, "path": self.path,
            "bytes": self.bytes, "reclaimable": self.refusal.is_none(),
            "refusal": self.refusal, "message": self.message, "references": self.references,
        })
    }
}

struct Walked {
    path: PathBuf,
    size: disk::TreeSize,
}

struct Scan {
    root: Option<PathBuf>,
    settings: disk::Settings,
    configuration_generation: u64,
    models: Vec<(String, Value, Vec<String>)>,
    inventory: Option<Arc<LocalInventory>>,
    transfers: Vec<JobView>,
    leftovers: Vec<QuarantinedFileView>,
    pulls: Vec<Value>,
    packages: Value,
    installing: bool,
    sessions: Option<Value>,
    import_roots: Vec<PathBuf>,
    artifact_root: Option<PathBuf>,
    journal_degraded: bool,
}

fn refusal_message(refusal: &str, references: &[String]) -> String {
    match refusal {
        "read_only" => "In a read-only library folder. Juiced never deletes there.".into(),
        "not_owned" => "Outside Juiced's own data folder, so Juiced won't delete it.".into(),
        "in_use" => format!("Used by {}.", references.join(", ")),
        "busy" => "Held by a running worker or download. Retry when it is idle.".into(),
        "active" => "Still downloading. Pause or cancel the download first.".into(),
        "publication_uncertain" => {
            "Saving may not have finished, so its space stays reserved until Juiced checks it."
                .into()
        }
        "install_running" => "An engine install is running. Retry when it finishes.".into(),
        "kept" => "Kept for rollback to v0.9.2 and for settings import.".into(),
        _ => "Not found. Refresh the storage view.".into(),
    }
}

fn refuse(mut candidate: Candidate, refusal: &'static str) -> Candidate {
    candidate.message = Some(refusal_message(refusal, &candidate.references));
    candidate.refusal = Some(refusal);
    candidate
}

fn strings_under(value: &Value, prefix: &str) -> bool {
    match value {
        Value::String(text) => text == prefix || text.starts_with(&format!("{prefix}/")),
        Value::Array(items) => items.iter().any(|item| strings_under(item, prefix)),
        Value::Object(map) => map.values().any(|item| strings_under(item, prefix)),
        _ => false,
    }
}

fn job_name(job: &JobView) -> String {
    job.source_name.clone().unwrap_or_else(|| {
        job.source_url
            .as_deref()
            .and_then(|url| url.split('?').next())
            .and_then(|url| url.rsplit('/').find(|part| !part.is_empty()))
            .unwrap_or("Download")
            .to_owned()
    })
}

impl Scan {
    fn pull_for(&self, job: &str) -> Option<&Value> {
        self.pulls.iter().find(|pull| {
            pull["jobs"]
                .as_array()
                .is_some_and(|jobs| jobs.iter().any(|known| known == job))
        })
    }

    fn file_candidates(&self) -> Vec<Candidate> {
        let Some(inventory) = &self.inventory else {
            return Vec::new();
        };
        inventory
            .groups
            .iter()
            .flat_map(|group| &group.files)
            .map(|file| {
                let candidate = Candidate {
                    id: format!("file:{}", file.id),
                    kind: "file",
                    title: file.name.clone(),
                    path: Some(file.location.display().to_string()),
                    bytes: file.bytes,
                    references: file.used_by.clone(),
                    ..Candidate::default()
                };
                match file.refusal {
                    None => candidate,
                    Some("read_only") => refuse(candidate, "read_only"),
                    Some("in_use") => refuse(candidate, "in_use"),
                    Some("not_owned") => refuse(candidate, "not_owned"),
                    Some(_) => refuse(candidate, "busy"),
                }
            })
            .collect()
    }

    fn partial_candidates(&self) -> Vec<Candidate> {
        self.transfers
            .iter()
            .filter(|job| !matches!(job.state, JobState::Complete | JobState::Cancelled))
            .map(|job| {
                let pull = self.pull_for(&job.id);
                let candidate = Candidate {
                    id: format!("partial:{}", job.id),
                    kind: "partial",
                    title: job_name(job),
                    bytes: job.downloaded_bytes,
                    references: pull
                        .map(|pull| {
                            let repo = pull["repo"].as_str().unwrap_or_default();
                            let label = pull["label"].as_str().unwrap_or_default();
                            vec![format!("download {repo} {label}").trim_end().to_owned()]
                        })
                        .unwrap_or_default(),
                    ..Candidate::default()
                };
                if job.writer_active
                    || matches!(
                        job.state,
                        JobState::Queued
                            | JobState::Downloading
                            | JobState::Pausing
                            | JobState::Cancelling
                    )
                {
                    refuse(candidate, "active")
                } else if job.state == JobState::Publishing
                    || job.error.as_deref() == Some("artifact_publication_uncertain")
                {
                    refuse(candidate, "publication_uncertain")
                } else {
                    candidate
                }
            })
            .collect()
    }

    fn engine_candidates(&self) -> Vec<Candidate> {
        let installed = crate::engines::installed_packages(&self.packages);
        self.packages["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|package| {
                let id = package["id"].as_str()?;
                let row = installed.iter().find(|row| row.package == id);
                let users: Vec<String> = self
                    .models
                    .iter()
                    .filter(|(_, _, packages)| packages.iter().any(|known| known == id))
                    .map(|(model, _, _)| model.clone())
                    .collect();
                let title = match row {
                    Some(row) => format!(
                        "{} {}",
                        row.engine.map_or("Engine", |engine| engine.name()),
                        row.backend.as_deref().unwrap_or("imported build")
                    ),
                    None => package["implementation_id"]
                        .as_str()
                        .unwrap_or("runtime package")
                        .to_owned(),
                };
                let candidate = Candidate {
                    id: format!("engine:{id}"),
                    kind: "engine",
                    title,
                    path: self
                        .root
                        .as_ref()
                        .map(|root| root.join(PACKAGE_STORE).join(id).display().to_string()),
                    bytes: package["reserved_bytes"].as_u64().unwrap_or(0),
                    references: users.clone(),
                    ..Candidate::default()
                };
                Some(if !users.is_empty() {
                    refuse(candidate, "in_use")
                } else if package["binding_leases"].as_u64().unwrap_or(0) > 0 {
                    refuse(candidate, "busy")
                } else if self.installing {
                    refuse(candidate, "install_running")
                } else {
                    candidate
                })
            })
            .collect()
    }

    fn legacy_candidates(&self, walked: &BTreeMap<String, Walked>) -> Vec<Candidate> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        LEGACY
            .iter()
            .filter_map(|(name, description, reclaimable)| {
                let path = root.join(name);
                let metadata = std::fs::symlink_metadata(&path).ok()?;
                if !metadata.is_dir() {
                    return None;
                }
                let prefix = path.display().to_string();
                let users: Vec<String> = self
                    .models
                    .iter()
                    .filter(|(_, spec, _)| strings_under(spec, &prefix))
                    .map(|(model, _, _)| model.clone())
                    .collect();
                let candidate = Candidate {
                    id: format!("legacy:{name}"),
                    kind: "legacy",
                    title: (*description).to_owned(),
                    path: Some(prefix),
                    bytes: walked.get(*name).map_or(0, |walked| walked.size.bytes),
                    references: users.clone(),
                    ..Candidate::default()
                };
                Some(if !reclaimable {
                    refuse(candidate, "kept")
                } else if !users.is_empty() {
                    refuse(candidate, "in_use")
                } else {
                    candidate
                })
            })
            .collect()
    }

    fn model_candidates(&self) -> Vec<Candidate> {
        let files: Vec<&crate::library::LocalInventoryFile> = self
            .inventory
            .iter()
            .flat_map(|inventory| &inventory.groups)
            .flat_map(|group| &group.files)
            .collect();
        self.models
            .iter()
            .map(|(model, _, _)| {
                let used: Vec<_> = files
                    .iter()
                    .filter(|file| file.used_by.iter().any(|user| user == model))
                    .collect();
                let exclusive: Vec<_> = used
                    .iter()
                    .filter(|file| {
                        file.used_by.len() == 1
                            && !file.read_only
                            && file.storage != LocalStorage::Local
                            && file.refusal != Some("not_owned")
                    })
                    .collect();
                let shared: BTreeSet<String> = used
                    .iter()
                    .flat_map(|file| file.used_by.iter())
                    .filter(|user| *user != model)
                    .cloned()
                    .collect();
                Candidate {
                    id: format!("model:{model}"),
                    kind: "model",
                    title: model.clone(),
                    bytes: exclusive.iter().map(|file| file.bytes).sum(),
                    references: shared.into_iter().collect(),
                    files: exclusive
                        .iter()
                        .filter(|file| file.storage == LocalStorage::HuggingfaceCache)
                        .map(|file| file.id.clone())
                        .collect(),
                    ..Candidate::default()
                }
            })
            .collect()
    }

    fn candidates(&self, walked: &BTreeMap<String, Walked>) -> Vec<Candidate> {
        let mut all = self.file_candidates();
        all.extend(self.partial_candidates());
        all.extend(self.engine_candidates());
        all.extend(self.legacy_candidates(walked));
        all.extend(self.model_candidates());
        all
    }
}

/// Changes whenever what could be reclaimed, or why not, changes. Partial byte counts are left
/// out so a running download does not invalidate every preview.
fn revision(candidates: &[Candidate]) -> String {
    let mut hasher = DefaultHasher::new();
    for candidate in candidates {
        candidate.id.hash(&mut hasher);
        candidate.refusal.hash(&mut hasher);
        candidate.references.hash(&mut hasher);
        if candidate.kind != "partial" {
            candidate.bytes.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn volumes(paths: &[(PathBuf, &'static str)], low_space: u64) -> Vec<Value> {
    let mut devices: Vec<(disk::Volume, Vec<String>, BTreeSet<&'static str>)> = Vec::new();
    for (path, role) in paths {
        let Some(existing) = disk::existing_ancestor(path) else {
            continue;
        };
        let Some(volume) = disk::volume(existing) else {
            continue;
        };
        let shown = path.display().to_string();
        match devices
            .iter_mut()
            .find(|(known, _, _)| known.device == volume.device)
        {
            Some((_, paths, roles)) => {
                if !paths.contains(&shown) {
                    paths.push(shown);
                }
                roles.insert(role);
            }
            None => devices.push((volume, vec![shown], BTreeSet::from([*role]))),
        }
    }
    devices
        .into_iter()
        .map(|(volume, paths, roles)| {
            let writable = !volume.read_only && !roles.iter().all(|role| *role == "library");
            json!({
                "path": paths[0], "paths": paths, "roles": roles,
                "total_bytes": volume.total_bytes, "available_bytes": volume.available_bytes,
                "used_bytes": volume.total_bytes.saturating_sub(volume.available_bytes),
                "read_only": volume.read_only,
                "low": writable && volume.available_bytes < low_space,
            })
        })
        .collect()
}

fn warnings(volumes: &[Value], sessions: Option<&Value>, journal_degraded: bool) -> Vec<Value> {
    let mut warnings: Vec<Value> = volumes
        .iter()
        .filter(|volume| volume["low"] == true)
        .map(|volume| {
            json!({
                "code": "low_space", "path": volume["path"],
                "available_bytes": volume["available_bytes"],
                "message": format!(
                    "{} free on {}. Downloads, installs and snapshots stop before the reserve.",
                    disk::human(volume["available_bytes"].as_u64().unwrap_or(0)),
                    volume["path"].as_str().unwrap_or_default(),
                ),
            })
        })
        .collect();
    if let Some(totals) = sessions.map(|sessions| &sessions["totals"])
        && totals["bytes"].as_u64() > totals["budget_bytes"].as_u64()
    {
        warnings.push(json!({
            "code": "session_cache_over_budget",
            "message": format!(
                "Saved sessions use {} of a {} budget. The oldest are evicted on the next save.",
                disk::human(totals["bytes"].as_u64().unwrap_or(0)),
                disk::human(totals["budget_bytes"].as_u64().unwrap_or(0)),
            ),
        }));
    }
    if journal_degraded {
        warnings.push(json!({
            "code": "acquisition_journal_unwritable",
            "message": "Download progress can't be recorded, so a restart may lose in-flight downloads.",
        }));
    }
    warnings
}

fn walk(root: &Path, names: &[&str]) -> BTreeMap<String, Walked> {
    let deadline = Instant::now() + WALK_DEADLINE;
    names
        .iter()
        .map(|name| {
            let path = root.join(name);
            let size = disk::tree_size(&path, WALK_ENTRIES, deadline);
            ((*name).to_owned(), Walked { path, size })
        })
        .collect()
}

fn cached_view() -> &'static Mutex<BTreeMap<PathBuf, (Instant, Value)>> {
    static CACHE: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, (Instant, Value)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn reclaim_lock() -> &'static AsyncMutex<()> {
    static LOCK: std::sync::OnceLock<AsyncMutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

impl Manager {
    /// Reloads journaled downloads before the service answers, so the first listing after a
    /// restart already shows them. Transfers continue in background tasks.
    pub async fn recover_acquisitions(self: &Arc<Self>) {
        if let Some(store) = self.artifacts.clone() {
            self.library.recover(store).await;
        }
        crate::native_api::recover(self.clone()).await;
    }

    fn storage_root(&self) -> Option<PathBuf> {
        self.config_root.as_ref().map(|root| root.root().to_owned())
    }

    /// Refuses before work starts when the volumes it writes would drop below the reserve.
    pub(super) async fn require_space(
        &self,
        needs: Vec<(PathBuf, u64)>,
        action: &'static str,
    ) -> Result<(), ApiError> {
        let reserve = self.config.lock().await.storage.reserve_bytes;
        tokio::task::spawn_blocking(move || {
            let needs: Vec<(&Path, u64)> = needs
                .iter()
                .map(|(path, bytes)| (path.as_path(), *bytes))
                .collect();
            disk::require(&needs, reserve)
        })
        .await
        .unwrap_or(Ok(()))
        .map_err(|shortfall| shortfall.into_error(action))
    }

    pub(super) fn cache_directory(&self) -> PathBuf {
        self.cache_directory.clone()
    }

    pub(super) fn package_directory(&self) -> Option<PathBuf> {
        self.storage_root().map(|root| root.join(PACKAGE_STORE))
    }

    async fn storage_scan(&self, fresh: bool) -> Scan {
        let config = self.config.lock().await.clone();
        let models = config
            .models
            .iter()
            .map(|model| {
                let profile = &model.execution_profile;
                (
                    model.id.clone(),
                    serde_json::to_value(model).unwrap_or(Value::Null),
                    [
                        profile.runtime_package(),
                        profile.previous_runtime_package(),
                    ]
                    .into_iter()
                    .flatten()
                    .map(str::to_owned)
                    .collect(),
                )
            })
            .collect();
        let (transfers, leftovers) = self
            .artifacts
            .as_ref()
            .and_then(|store| store.transfers().ok())
            .unwrap_or_default();
        let pulls = self.library.pulls_view()["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let packages = self
            .runtime_packages
            .get()
            .and_then(|store| store.inventory().ok())
            .unwrap_or(Value::Null);
        let installing = self
            .engine_jobs
            .views()
            .iter()
            .any(|job| !matches!(job["state"].as_str(), Some("complete" | "failed")));
        Scan {
            root: self.storage_root(),
            settings: config.storage.clone(),
            configuration_generation: config.generation,
            models,
            inventory: self.local_library(fresh).await.ok(),
            transfers,
            leftovers,
            pulls,
            packages,
            installing,
            sessions: self.sessions_view().await.ok(),
            import_roots: self.canonical_import_roots().await,
            artifact_root: self.artifacts.as_ref().map(|store| store.root().to_owned()),
            journal_degraded: self
                .library
                .journal()
                .is_some_and(|journal| journal.degraded()),
        }
    }

    fn volume_paths(&self, scan: &Scan) -> Vec<(PathBuf, &'static str)> {
        let mut paths = Vec::new();
        if let Some(root) = &scan.root {
            paths.push((root.clone(), "data"));
        }
        if let Some(root) = &scan.artifact_root {
            paths.push((root.clone(), "models"));
        }
        for hub in scan.inventory.iter().flat_map(|inventory| &inventory.hubs) {
            paths.push((
                hub.path.clone(),
                if hub.read_only { "library" } else { "models" },
            ));
        }
        if let Some(packages) = self.package_directory() {
            paths.push((packages, "engines"));
        }
        paths.push((self.cache_directory(), "session_cache"));
        for root in &scan.import_roots {
            if scan
                .root
                .as_ref()
                .is_none_or(|owned| !root.starts_with(owned))
            {
                paths.push((root.clone(), "library"));
            }
        }
        paths
    }

    /// The storage view. `summary` skips directory walks and per-file detail, for frequent
    /// polling such as the Dashboard.
    pub async fn storage_view(&self, summary: bool) -> Result<Value, ApiError> {
        if !summary
            && let Some((at, view)) = cached_view()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&self.config_path)
            && at.elapsed() < VIEW_TTL
        {
            return Ok(view.clone());
        }
        let started = Instant::now();
        let scan = self.storage_scan(false).await;
        let paths = self.volume_paths(&scan);
        let low_space = scan.settings.low_space_bytes;
        let volume_rows = tokio::task::spawn_blocking(move || volumes(&paths, low_space))
            .await
            .unwrap_or_default();
        let warnings = warnings(&volume_rows, scan.sessions.as_ref(), scan.journal_degraded);
        if summary {
            return Ok(json!({
                "schema_version": 1, "summary": true, "settings": scan.settings,
                "volumes": volume_rows, "warnings": warnings,
            }));
        }
        let root = scan.root.clone();
        let hub_names: BTreeSet<String> = scan
            .inventory
            .iter()
            .flat_map(|inventory| &inventory.hubs)
            .filter_map(|hub| {
                let root = root.as_ref()?;
                let relative = hub.path.strip_prefix(root).ok()?;
                relative
                    .components()
                    .next()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
            })
            .collect();
        let walked = match root.clone() {
            Some(root) => tokio::task::spawn_blocking(move || {
                let mut names: Vec<String> = std::fs::read_dir(&root)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name != "artifacts" && !hub_names.contains(name))
                    .collect();
                names.sort();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                walk(&root, &names)
            })
            .await
            .unwrap_or_default(),
            None => BTreeMap::new(),
        };
        let candidates = scan.candidates(&walked);
        let revision = revision(&candidates);
        let view = self.compose_view(&scan, &walked, &candidates, revision, volume_rows, warnings);
        if started.elapsed() > SLOW_VIEW {
            cached_view()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(self.config_path.clone(), (Instant::now(), view.clone()));
        }
        Ok(view)
    }

    fn compose_view(
        &self,
        scan: &Scan,
        walked: &BTreeMap<String, Walked>,
        candidates: &[Candidate],
        revision: String,
        volumes: Vec<Value>,
        warnings: Vec<Value>,
    ) -> Value {
        let of = |kind: &'static str| candidates.iter().filter(move |item| item.kind == kind);
        let files: Vec<&crate::library::LocalInventoryFile> = scan
            .inventory
            .iter()
            .flat_map(|inventory| &inventory.groups)
            .flat_map(|group| &group.files)
            .collect();
        let sum = |filter: &dyn Fn(&&&crate::library::LocalInventoryFile) -> bool| -> u64 {
            files.iter().filter(filter).map(|file| file.bytes).sum()
        };
        let owned = sum(&|file| !file.read_only && file.storage != LocalStorage::Local);
        let library = sum(&|file| file.read_only);
        let elsewhere = sum(&|file| !file.read_only && file.storage == LocalStorage::Local);
        let walked_sum = |names: &[&str]| -> (u64, bool) {
            names.iter().fold((0, false), |(bytes, truncated), name| {
                walked.get(*name).map_or((bytes, truncated), |walked| {
                    (
                        bytes + walked.size.bytes,
                        truncated || walked.size.truncated,
                    )
                })
            })
        };
        let (engine_bytes, engine_truncated) = walked_sum(ENGINE_DIRECTORIES);
        let (config_bytes, _) = walked_sum(CONFIG_ENTRIES);
        let cache_name = self
            .cache_directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cache_size = walked
            .get(&cache_name)
            .map(|walked| walked.size)
            .unwrap_or_else(|| {
                disk::tree_size(
                    &self.cache_directory,
                    WALK_ENTRIES,
                    Instant::now() + WALK_DEADLINE,
                )
            });
        let totals = scan.sessions.as_ref().map(|sessions| &sessions["totals"]);
        let session_bytes = totals.and_then(|totals| totals["bytes"].as_u64());
        let partial_bytes: u64 = of("partial").map(|item| item.bytes).sum::<u64>()
            + scan
                .leftovers
                .iter()
                .map(|leftover| leftover.reserved_bytes)
                .sum::<u64>();
        let legacy_bytes: u64 = of("legacy").map(|item| item.bytes).sum();
        let known: BTreeSet<&str> = ENGINE_DIRECTORIES
            .iter()
            .chain(CONFIG_ENTRIES)
            .copied()
            .chain(LEGACY.iter().map(|(name, _, _)| *name))
            .chain(std::iter::once(cache_name.as_str()))
            .collect();
        let other: Vec<Value> = walked
            .iter()
            .filter(|(name, _)| !known.contains(name.as_str()))
            .map(|(name, walked)| {
                json!({
                    "name": name, "path": walked.path, "bytes": walked.size.bytes,
                    "files": walked.size.files, "truncated": walked.size.truncated,
                })
            })
            .collect();
        let other_bytes: u64 = other.iter().filter_map(|row| row["bytes"].as_u64()).sum();
        let reclaimable = |kind: &'static str| -> u64 {
            of(kind)
                .filter(|item| item.refusal.is_none())
                .map(|item| item.bytes)
                .sum()
        };
        let categories = json!([
            {"id": "models", "title": "Model weights", "bytes": owned + elsewhere, "reclaimable_bytes": reclaimable("file"),
             "detail": {"owned_bytes": owned, "elsewhere_bytes": elsewhere, "library_bytes": library}},
            {"id": "library", "title": "Read-only library", "bytes": library, "reclaimable_bytes": 0},
            {"id": "downloads", "title": "Partial downloads", "bytes": partial_bytes, "reclaimable_bytes": reclaimable("partial")},
            {"id": "engines", "title": "Engines", "bytes": engine_bytes, "truncated": engine_truncated, "reclaimable_bytes": reclaimable("engine")},
            {"id": "session_cache", "title": "Session cache", "bytes": cache_size.bytes, "truncated": cache_size.truncated, "reclaimable_bytes": 0},
            {"id": "config", "title": "Settings", "bytes": config_bytes, "reclaimable_bytes": 0},
            {"id": "logs", "title": "Logs", "bytes": 0, "reclaimable_bytes": 0, "detail": {"persisted": false}},
            {"id": "legacy", "title": "v0.9.2 leftovers", "bytes": legacy_bytes, "reclaimable_bytes": reclaimable("legacy")},
            {"id": "other", "title": "Other", "bytes": other_bytes, "reclaimable_bytes": 0},
        ]);
        let mut models: Vec<Value> = scan
            .models
            .iter()
            .map(|(model, _, packages)| {
                let used: Vec<_> = files
                    .iter()
                    .filter(|file| file.used_by.iter().any(|user| user == model))
                    .collect();
                let removal = candidates
                    .iter()
                    .find(|item| item.kind == "model" && item.title == *model);
                json!({
                    "model": model, "id": format!("model:{model}"),
                    "bytes": used.iter().map(|file| file.bytes).sum::<u64>(),
                    "files": used.len(),
                    "shared_bytes": used.iter().filter(|file| file.used_by.len() > 1).map(|file| file.bytes).sum::<u64>(),
                    "read_only_bytes": used.iter().filter(|file| file.read_only).map(|file| file.bytes).sum::<u64>(),
                    "removal_frees_bytes": removal.map_or(0, |item| item.bytes),
                    "shared_with": removal.map(|item| item.references.clone()).unwrap_or_default(),
                    "engines": packages,
                })
            })
            .collect();
        models.sort_by(|left, right| right["bytes"].as_u64().cmp(&left["bytes"].as_u64()));
        let mut quant_users: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for group in scan
            .inventory
            .iter()
            .flat_map(|inventory| &inventory.groups)
        {
            for quant in &group.quants {
                for file in &quant.files {
                    quant_users
                        .entry(file.as_str())
                        .or_default()
                        .push(format!("{} {}", group.name, quant.label));
                }
            }
        }
        let shared: Vec<Value> = files
            .iter()
            .filter(|file| {
                file.used_by.len() > 1
                    || quant_users
                        .get(file.id.as_str())
                        .is_some_and(|quants| quants.len() > 1)
            })
            .map(|file| {
                json!({
                    "id": format!("file:{}", file.id), "name": file.name,
                    "path": file.location, "bytes": file.bytes, "used_by": file.used_by,
                    "quants": quant_users.get(file.id.as_str()).cloned().unwrap_or_default(),
                    "read_only": file.read_only,
                })
            })
            .collect();
        let unreferenced: Vec<Value> = of("file")
            .filter(|item| item.refusal.is_none())
            .map(Candidate::json)
            .collect();
        let engine_rows: Vec<Value> = of("engine")
            .map(|item| {
                let mut row = item.json();
                row["used_by"] = json!(item.references);
                row["unused"] = json!(item.references.is_empty());
                row
            })
            .collect();
        json!({
            "schema_version": 1, "summary": false, "revision": revision,
            "configuration_generation": scan.configuration_generation,
            "generated_at_unix_ms": unix_time_ms(), "settings": scan.settings,
            "root": scan.root,
            "volumes": volumes, "warnings": warnings, "categories": categories,
            "models": models, "shared": shared, "unreferenced": unreferenced,
            "partials": of("partial").map(Candidate::json).collect::<Vec<_>>(),
            "leftover_files": scan.leftovers.iter().map(|leftover| json!({
                "filename": leftover.filename, "reserved_bytes": leftover.reserved_bytes,
                "reclaimable": false, "message": "Left by an interrupted download. Juiced checks it on the next restart.",
            })).collect::<Vec<_>>(),
            "engines": engine_rows,
            "session_cache": {
                "path": self.cache_directory, "bytes": cache_size.bytes,
                "session_bytes": session_bytes,
                "snapshot_bytes": session_bytes.map(|sessions| cache_size.bytes.saturating_sub(sessions)),
                "sessions": totals.and_then(|totals| totals["sessions"].as_u64()),
                "budget_bytes": totals.and_then(|totals| totals["budget_bytes"].as_u64()),
                "over_budget": totals.is_some_and(|totals| totals["bytes"].as_u64() > totals["budget_bytes"].as_u64()),
            },
            "legacy": of("legacy").map(Candidate::json).collect::<Vec<_>>(),
            "other": other,
            "truncated": scan.inventory.as_ref().is_some_and(|inventory| inventory.truncated)
                || walked.values().any(|walked| walked.size.truncated),
        })
    }

    pub async fn storage_reclaim(self: &Arc<Self>, request: Reclaim) -> Result<Value, ApiError> {
        let mut items = request.items.clone();
        items.dedup();
        if items.is_empty() || items.len() > MAXIMUM_RECLAIM {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_storage_reclaim",
                format!("Name between 1 and {MAXIMUM_RECLAIM} items from GET /juiced/storage."),
            ));
        }
        if !request.preview && request.expected_revision.is_none() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "storage_revision_required",
                "Preview first, then pass its revision as expected_revision.",
            ));
        }
        let _serialized = reclaim_lock().lock().await;
        let scan = self.storage_scan(true).await;
        let walked = match &scan.root {
            Some(root) => {
                let root = root.clone();
                let names: Vec<&'static str> = LEGACY.iter().map(|(name, _, _)| *name).collect();
                tokio::task::spawn_blocking(move || walk(&root, &names))
                    .await
                    .unwrap_or_default()
            }
            None => BTreeMap::new(),
        };
        let candidates = scan.candidates(&walked);
        let current = revision(&candidates);
        let selected: Vec<Candidate> = items
            .iter()
            .map(|id| {
                candidates
                    .iter()
                    .find(|candidate| candidate.id == *id)
                    .cloned()
                    .unwrap_or_else(|| {
                        refuse(
                            Candidate {
                                id: id.clone(),
                                kind: id.split(':').next().map_or("unknown", |kind| match kind {
                                    "file" => "file",
                                    "partial" => "partial",
                                    "engine" => "engine",
                                    "legacy" => "legacy",
                                    "model" => "model",
                                    _ => "unknown",
                                }),
                                title: id.clone(),
                                ..Candidate::default()
                            },
                            "not_found",
                        )
                    })
            })
            .collect();
        let freed: u64 = selected
            .iter()
            .filter(|item| item.refusal.is_none())
            .map(|item| item.bytes)
            .sum();
        let preview = json!({
            "schema_version": 1, "preview": true, "applied": false, "revision": current,
            "freed_bytes": freed,
            "refused": selected.iter().filter(|item| item.refusal.is_some()).count(),
            "items": selected.iter().map(Candidate::json).collect::<Vec<_>>(),
        });
        if request.preview {
            return Ok(preview);
        }
        if request.expected_revision.as_deref() != Some(current.as_str()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "storage_revision_conflict",
                "Storage changed since the preview. Preview again before reclaiming.",
            ));
        }
        if let Some(refused) = selected.iter().find(|item| item.refusal.is_some()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "storage_item_refused",
                format!(
                    "{}: {}",
                    refused.title,
                    refused.message.clone().unwrap_or_default()
                ),
            ));
        }
        let mut results = Vec::with_capacity(selected.len());
        let mut total = 0u64;
        for item in &selected {
            let outcome = self.reclaim_one(item).await;
            let (applied, bytes, error) = match outcome {
                Ok(bytes) => (true, bytes, None),
                Err(failure) => (
                    false,
                    0,
                    Some(json!({"code": failure.code, "message": failure.message})),
                ),
            };
            total = total.saturating_add(bytes);
            results.push(json!({
                "id": item.id, "kind": item.kind, "title": item.title,
                "applied": applied, "freed_bytes": bytes, "error": error,
            }));
        }
        self.library.invalidate_local();
        cached_view()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.config_path);
        Ok(json!({
            "schema_version": 1, "preview": false, "applied": results.iter().all(|row| row["applied"] == true),
            "freed_bytes": total, "items": results,
        }))
    }

    async fn reclaim_one(self: &Arc<Self>, item: &Candidate) -> Result<u64, ApiError> {
        let (_, key) = item.id.split_once(':').unwrap_or_default();
        match item.kind {
            "file" => {
                let deleted = self
                    .delete_local_files(LocalDelete {
                        files: vec![key.to_owned()],
                    })
                    .await?;
                Ok(deleted["freed_bytes"].as_u64().unwrap_or(0))
            }
            "partial" => {
                let store = self.artifacts()?;
                let id = key.to_owned();
                tokio::task::spawn_blocking(move || {
                    let job = store.job(&id)?;
                    let bytes = job.downloaded_bytes;
                    let job = if job.state == JobState::Cancelled {
                        job
                    } else {
                        store.cancel(&id, job.revision)?
                    };
                    let settled = if job.writer_active {
                        let deadline = Instant::now() + Duration::from_secs(5);
                        loop {
                            let current = store.job(&id)?;
                            if !current.writer_active || Instant::now() > deadline {
                                break current;
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    } else {
                        job
                    };
                    store.forget_job(&id, settled.revision)?;
                    Ok::<_, crate::artifacts::ArtifactError>(bytes)
                })
                .await
                .map_err(|_| reclaim_interrupted())?
                .map_err(ApiError::from)
            }
            "engine" => {
                let store = self.runtime_packages.get().cloned().ok_or_else(|| {
                    ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "runtime_packages_unavailable",
                        "Installed engine storage is unavailable.",
                    )
                })?;
                // Holding the configuration keeps a model from binding the package between
                // the usage check and its removal.
                let config = self.config.lock().await;
                if config.models.iter().any(|model| {
                    let profile = &model.execution_profile;
                    [
                        profile.runtime_package(),
                        profile.previous_runtime_package(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|package| package == key)
                }) {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "engine_in_use",
                        "A model started using this engine. Preview again.",
                    ));
                }
                let id = key.to_owned();
                let freed = tokio::task::spawn_blocking(move || {
                    let generation = store
                        .inventory()
                        .ok()
                        .and_then(|inventory| inventory["generation"].as_u64())
                        .unwrap_or_default();
                    store.remove(&id, generation)
                })
                .await
                .map_err(|_| reclaim_interrupted())?
                .map_err(|error| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        error.code(),
                        format!("The engine wasn't removed ({}).", error.code()),
                    )
                })?;
                drop(config);
                Ok(freed)
            }
            "legacy" => {
                let root = self.storage_root().ok_or_else(reclaim_interrupted)?;
                let config = self.config.lock().await;
                let path = root.join(key);
                let prefix = path.display().to_string();
                if config.models.iter().any(|model| {
                    strings_under(&serde_json::to_value(model).unwrap_or(Value::Null), &prefix)
                }) {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "legacy_in_use",
                        "A model now uses this folder. Preview again.",
                    ));
                }
                let bytes = item.bytes;
                tokio::task::spawn_blocking(move || {
                    let metadata = std::fs::symlink_metadata(&path)?;
                    if !metadata.is_dir() {
                        return Err(std::io::Error::other("not a directory"));
                    }
                    disk::remove_tree(&path)
                })
                .await
                .map_err(|_| reclaim_interrupted())?
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "legacy_delete_failed",
                        format!("{prefix} couldn't be fully deleted. Check its permissions."),
                    )
                })?;
                drop(config);
                Ok(bytes)
            }
            "model" => {
                let removed = crate::native_api::delete_model(self, key).await?;
                let mut freed = removed["freed_bytes"].as_u64().unwrap_or(0);
                if !item.files.is_empty()
                    && let Ok(deleted) = self
                        .delete_local_files(LocalDelete {
                            files: item.files.clone(),
                        })
                        .await
                {
                    freed += deleted["freed_bytes"].as_u64().unwrap_or(0);
                }
                Ok(freed)
            }
            _ => Err(reclaim_interrupted()),
        }
    }
}

fn reclaim_interrupted() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "storage_reclaim_interrupted",
        "Reclaiming was interrupted. Refresh the storage view before retrying.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_reference_check_matches_whole_path_components_only() {
        let spec = json!({"model_file": "/data/cache/llama/model.gguf", "args": ["--x", "/data/cache-two"]});
        assert!(strings_under(&spec, "/data/cache"));
        assert!(!strings_under(
            &json!({"a": "/data/cache-two"}),
            "/data/cache"
        ));
        assert!(strings_under(
            &json!(["/data/prompt-state"]),
            "/data/prompt-state"
        ));
    }

    #[test]
    fn revision_ignores_partial_progress_but_tracks_refusals() {
        let partial = Candidate {
            id: "partial:a".into(),
            kind: "partial",
            bytes: 1,
            ..Candidate::default()
        };
        let mut grown = partial.clone();
        grown.bytes = 99;
        assert_eq!(revision(std::slice::from_ref(&partial)), revision(&[grown]));
        let refused = refuse(partial.clone(), "active");
        assert_ne!(revision(&[partial]), revision(&[refused]));
    }
}
