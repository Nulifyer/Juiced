use super::quant;
use crate::artifacts::{ArtifactRole, StoreListing};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub const MAXIMUM_GROUPS: usize = 512;
pub const MAXIMUM_FILES: usize = 4096;
const MAXIMUM_REPOS_PER_HUB: usize = 1024;
const MAXIMUM_SNAPSHOTS: usize = 8;
pub const STORE_LISTING: usize = 2048;

/// Splits a Hugging Face resolve URL into repository, revision and repository-relative path.
pub type ResolveUrl<'a> = dyn Fn(&str) -> Option<(String, String, String)> + 'a;
/// Label, primary file id, member ids, part count and completeness of one quant.
type DraftVariant = (Option<String>, String, Vec<String>, u32, bool);

#[derive(Clone, Debug)]
pub struct Hub {
    pub path: PathBuf,
    pub owned: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct Registered {
    pub id: String,
    pub paths: Vec<PathBuf>,
    pub artifact: Option<String>,
    pub artifact_set: Option<String>,
}

pub struct ScanInputs {
    pub hubs: Vec<Hub>,
    pub roots: Vec<PathBuf>,
    pub store: Option<StoreListing>,
    pub store_read_only: bool,
    pub registrations: Vec<Registered>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Storage {
    HuggingfaceCache,
    ArtifactStore,
    Local,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalFile {
    pub id: String,
    pub name: String,
    pub path: String,
    pub quant: Option<String>,
    pub role: &'static str,
    pub bytes: u64,
    pub location: PathBuf,
    pub storage: Storage,
    pub revision: Option<String>,
    pub artifact: Option<String>,
    pub read_only: bool,
    pub removable: bool,
    pub refusal: Option<&'static str>,
    pub used_by: Vec<String>,
    #[serde(skip)]
    pub canonical: PathBuf,
    #[serde(skip)]
    pub digest: Option<String>,
    #[serde(skip)]
    pub repo_directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalQuant {
    pub label: String,
    pub file: String,
    pub parts: u32,
    pub complete: bool,
    pub bytes: u64,
    pub files: Vec<String>,
    pub used_by: Vec<String>,
    pub removable: bool,
    pub add: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalGroup {
    pub id: String,
    pub repo: Option<String>,
    pub name: String,
    pub publisher: Option<String>,
    pub storage: Vec<Storage>,
    pub location: Option<PathBuf>,
    pub read_only: bool,
    pub bytes: u64,
    pub removable_bytes: u64,
    pub used_by: Vec<String>,
    pub quants: Vec<LocalQuant>,
    pub files: Vec<LocalFile>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HubView {
    pub path: PathBuf,
    pub owned: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalInventory {
    pub schema_version: u32,
    pub artifact_generation: Option<u64>,
    pub total_bytes: u64,
    pub removable_bytes: u64,
    pub truncated: bool,
    pub hubs: Vec<HubView>,
    pub groups: Vec<LocalGroup>,
}

impl LocalInventory {
    pub fn file(&self, id: &str) -> Option<&LocalFile> {
        self.groups
            .iter()
            .flat_map(|group| &group.files)
            .find(|file| file.id == id)
    }

    /// Hugging Face cache files keyed by repository and repository-relative path, so search can
    /// mark adopted weights as present without hashing them.
    pub fn cache_files(&self) -> impl Iterator<Item = (&str, &LocalFile)> {
        self.groups.iter().flat_map(|group| {
            group
                .files
                .iter()
                .filter(|file| file.storage == Storage::HuggingfaceCache)
                .filter_map(move |file| Some((group.repo.as_deref()?, file)))
        })
    }
}

pub fn writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(text) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `text` is a valid NUL-terminated path for the duration of the call.
        unsafe { libc::access(text.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).is_ok_and(|metadata| !metadata.permissions().readonly())
    }
}

const WEIGHT_SUFFIXES: &[&str] = &[
    ".gguf",
    ".bin",
    ".safetensors",
    ".onnx",
    ".ort",
    ".pt",
    ".pth",
    ".ckpt",
    ".npz",
];

fn listed(path: &str) -> bool {
    let name = quant::basename(path).to_ascii_lowercase();
    WEIGHT_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        || name.starts_with("voices")
        || matches!(
            name.as_str(),
            "streaming_config.json" | "tokenizer.json" | "index.json"
        )
}

fn role_name(role: ArtifactRole) -> &'static str {
    match role {
        ArtifactRole::Model | ArtifactRole::DraftModel => "model",
        ArtifactRole::WeightShard => "shard",
        ArtifactRole::Projector => "projector",
        ArtifactRole::Vae => "vae",
        ArtifactRole::Encoder => "text_encoder",
        ArtifactRole::Voice | ArtifactRole::VoiceConfiguration => "voices",
        ArtifactRole::Tokenizer | ArtifactRole::Vocabulary => "tokenizer",
        ArtifactRole::Configuration | ArtifactRole::Processor | ArtifactRole::WeightIndex => {
            "config"
        }
        ArtifactRole::Adapter
        | ArtifactRole::ExternalTensor
        | ArtifactRole::Decoder
        | ArtifactRole::Denoiser => "model",
    }
}

fn role_of(path: &str) -> &'static str {
    let name = quant::basename(path).to_ascii_lowercase();
    let lower = path.to_ascii_lowercase();
    if name.contains("mmproj") {
        "projector"
    } else if quant::split_part(path).is_some() {
        "shard"
    } else if name.starts_with("voices") {
        "voices"
    } else if lower.contains("vae/") || name.contains("vae") || name.starts_with("ae.") {
        "vae"
    } else if lower.contains("text_encoder")
        || ["t5xxl", "umt5", "clip_l", "clip_g", "clip-l", "clip-g"]
            .iter()
            .any(|marker| name.contains(marker))
    {
        "text_encoder"
    } else if name.ends_with(".json") && name.contains("token") {
        "tokenizer"
    } else if name.ends_with(".json") {
        "config"
    } else {
        "model"
    }
}

fn label_for(path: &str) -> Option<String> {
    quant::quant_label(quant::basename(path))
}

fn extension_label(path: &str) -> String {
    quant::basename(path)
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty() && extension.len() <= 16)
        .map_or_else(|| "File".to_owned(), str::to_ascii_uppercase)
}

/// Display name of a loose file: its stem without a trailing quant label.
pub fn display_stem(name: &str) -> String {
    let base = quant::basename(name);
    let base = quant::split_part(base).map_or(base, |(prefix, _, _)| prefix);
    let stem = base
        .rsplit_once('.')
        .filter(|(_, extension)| {
            WEIGHT_SUFFIXES
                .iter()
                .any(|suffix| suffix[1..].eq_ignore_ascii_case(extension))
        })
        .map_or(base, |(stem, _)| stem);
    if let Some(label) = quant::quant_label(stem)
        && stem.to_ascii_uppercase().ends_with(&label)
    {
        let cut = stem.len() - label.len();
        if cut > 1 && b"-._".contains(&stem.as_bytes()[cut - 1]) {
            return stem[..cut - 1].to_owned();
        }
    }
    stem.to_owned()
}

fn url_file_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    (!name.is_empty() && !path.ends_with("//")).then(|| name.to_owned())
}

fn cache_repo(directory: &str) -> Option<String> {
    let rest = directory.strip_prefix("models--")?;
    let (owner, name) = rest.split_once("--")?;
    let repo = format!("{owner}/{name}");
    super::valid_repo(&repo).then_some(repo)
}

fn blob_digest(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    (name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| name.to_owned())
}

struct Draft {
    repo: Option<String>,
    name: String,
    publisher: Option<String>,
    location: Option<PathBuf>,
    files: Vec<LocalFile>,
    sets: Vec<(String, Vec<String>)>,
}

impl Draft {
    fn new(repo: Option<String>, name: String, location: Option<PathBuf>) -> Self {
        let publisher = repo
            .as_deref()
            .and_then(|repo| repo.split_once('/'))
            .map(|(owner, _)| owner.to_owned());
        Self {
            repo,
            name,
            publisher,
            location,
            files: Vec::new(),
            sets: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Drafts {
    order: BTreeMap<String, Draft>,
    seen: BTreeSet<PathBuf>,
    files: usize,
    truncated: bool,
}

impl Drafts {
    fn entry(&mut self, key: String, make: impl FnOnce() -> Draft) -> Option<&mut Draft> {
        if !self.order.contains_key(&key) && self.order.len() >= MAXIMUM_GROUPS {
            self.truncated = true;
            return None;
        }
        Some(self.order.entry(key).or_insert_with(make))
    }

    fn admit(&mut self, canonical: &Path) -> bool {
        if self.seen.contains(canonical) {
            return false;
        }
        if self.files >= MAXIMUM_FILES {
            self.truncated = true;
            return false;
        }
        self.files += 1;
        self.seen.insert(canonical.to_owned());
        true
    }
}

fn hf_key(repo: &str) -> String {
    format!("hf:{repo}")
}

fn scan_hub(hub: &Hub, roots: &[PathBuf], drafts: &mut Drafts) {
    let Ok(entries) = std::fs::read_dir(&hub.path) else {
        return;
    };
    let mut repos: Vec<(String, PathBuf)> = entries
        .flatten()
        .take(MAXIMUM_REPOS_PER_HUB)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((cache_repo(&name)?, entry.path()))
        })
        .collect();
    repos.sort();
    for (repo, directory) in repos {
        for snapshot in crate::native_import::snapshots(&directory, None)
            .into_iter()
            .take(MAXIMUM_SNAPSHOTS)
        {
            let root = directory.join("snapshots").join(&snapshot);
            for (relative, canonical, bytes) in crate::native_import::list_files(&root, roots) {
                if !listed(&relative) || !drafts.admit(&canonical) {
                    continue;
                }
                let name = quant::basename(&relative).to_owned();
                let location = root.join(&relative);
                let file = LocalFile {
                    id: location.to_string_lossy().into_owned(),
                    quant: label_for(&relative),
                    role: role_of(&relative),
                    name: name.clone(),
                    path: relative,
                    bytes,
                    location,
                    storage: Storage::HuggingfaceCache,
                    revision: Some(snapshot.clone()),
                    artifact: None,
                    read_only: hub.read_only,
                    removable: false,
                    refusal: None,
                    used_by: Vec::new(),
                    digest: blob_digest(&canonical),
                    canonical,
                    repo_directory: Some(directory.clone()),
                };
                let Some(draft) = drafts.entry(hf_key(&repo), || {
                    let name = repo.split_once('/').map_or(repo.as_str(), |(_, name)| name);
                    Draft::new(Some(repo.clone()), name.to_owned(), Some(directory.clone()))
                }) else {
                    return;
                };
                draft.files.push(file);
            }
        }
    }
}

fn scan_store(store: &StoreListing, read_only: bool, parse: &ResolveUrl<'_>, drafts: &mut Drafts) {
    let mut membership: BTreeMap<&str, (&str, ArtifactRole, &str)> = BTreeMap::new();
    for set in &store.sets {
        for member in &set.specification.members {
            membership.entry(member.artifact.as_str()).or_insert((
                set.id.as_str(),
                member.role,
                member.name.as_str(),
            ));
        }
    }
    let mut keys: BTreeMap<&str, String> = BTreeMap::new();
    for file in store.files.iter().filter(|file| !file.removing) {
        let source = file.source_url.as_deref().and_then(parse);
        let member = membership.get(file.id.as_str());
        let name = file
            .source_name
            .clone()
            .or_else(|| {
                source
                    .as_ref()
                    .map(|(_, _, path)| quant::basename(path).to_owned())
            })
            .or_else(|| member.map(|(_, _, name)| (*name).to_owned()))
            .or_else(|| file.source_url.as_deref().and_then(url_file_name))
            .unwrap_or_else(|| format!("{}.bin", &file.id[..12.min(file.id.len())]));
        let path = source
            .as_ref()
            .map_or_else(|| name.clone(), |(_, _, path)| path.clone());
        let canonical = std::fs::canonicalize(&file.path).unwrap_or_else(|_| file.path.clone());
        if !drafts.admit(&canonical) {
            continue;
        }
        let key = match (&source, member) {
            (Some((repo, _, _)), _) => hf_key(repo),
            (None, Some((set, _, _))) => keys
                .get(set)
                .cloned()
                .unwrap_or_else(|| format!("store:{}", display_stem(&name).to_lowercase())),
            (None, None) => format!("store:{}", display_stem(&name).to_lowercase()),
        };
        if let Some((set, ..)) = member {
            keys.entry(set).or_insert_with(|| key.clone());
        }
        let local = LocalFile {
            id: format!("artifact:{}", file.id),
            quant: label_for(&path),
            role: member.map_or_else(|| role_of(&path), |(_, role, _)| role_name(*role)),
            name: name.clone(),
            path,
            bytes: file.bytes,
            location: file.path.clone(),
            storage: Storage::ArtifactStore,
            revision: source.as_ref().map(|(_, revision, _)| revision.clone()),
            artifact: Some(file.id.clone()),
            read_only,
            removable: false,
            refusal: None,
            used_by: Vec::new(),
            canonical,
            digest: Some(file.id.clone()),
            repo_directory: None,
        };
        let repo = source.as_ref().map(|(repo, _, _)| repo.clone());
        let Some(draft) = drafts.entry(key, || match &repo {
            Some(repo) => {
                let short = repo.split_once('/').map_or(repo.as_str(), |(_, name)| name);
                Draft::new(Some(repo.clone()), short.to_owned(), None)
            }
            None => Draft::new(None, display_stem(&name), None),
        }) else {
            return;
        };
        draft.files.push(local);
    }
    for set in &store.sets {
        let ids: Vec<String> = set
            .specification
            .members
            .iter()
            .map(|member| format!("artifact:{}", member.artifact))
            .collect();
        if let Some(draft) = drafts
            .order
            .values_mut()
            .find(|draft| draft.files.iter().any(|file| ids.contains(&file.id)))
        {
            draft.sets.push((set.id.clone(), ids));
        }
    }
}

fn scan_registrations(registrations: &[Registered], drafts: &mut Drafts) {
    for registration in registrations {
        let Some(model) = registration.paths.first() else {
            continue;
        };
        let Ok(canonical_model) = std::fs::canonicalize(model) else {
            continue;
        };
        let parent = canonical_model.parent().map(Path::to_owned);
        let model_name = model
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let key = format!(
            "path:{}:{}",
            parent.as_deref().unwrap_or(Path::new("")).display(),
            display_stem(&model_name).to_lowercase()
        );
        for path in &registration.paths {
            let Ok(canonical) = std::fs::canonicalize(path) else {
                continue;
            };
            let Ok(metadata) = std::fs::metadata(&canonical) else {
                continue;
            };
            if !metadata.is_file() || !drafts.admit(&canonical) {
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let read_only = !canonical.parent().is_some_and(writable);
            let file = LocalFile {
                id: path.to_string_lossy().into_owned(),
                quant: label_for(&name),
                role: role_of(&name),
                path: name.clone(),
                name,
                bytes: metadata.len(),
                location: path.clone(),
                storage: Storage::Local,
                revision: None,
                artifact: None,
                read_only,
                removable: false,
                refusal: None,
                used_by: Vec::new(),
                canonical,
                digest: None,
                repo_directory: None,
            };
            let Some(draft) = drafts.entry(key.clone(), || {
                Draft::new(None, display_stem(&model_name), parent.clone())
            }) else {
                return;
            };
            draft.files.push(file);
        }
    }
}

fn finish(draft: Draft, store_in_use: &BTreeSet<String>) -> LocalGroup {
    let Draft {
        repo,
        name,
        publisher,
        location,
        files,
        sets,
    } = draft;
    let metas: Vec<quant::FileMeta> = files
        .iter()
        .filter(|file| file.role == "model" || file.role == "shard")
        .map(|file| quant::FileMeta {
            path: file.id.clone(),
            bytes: Some(file.bytes),
            sha256: None,
        })
        .collect();
    fn path_of<'a>(files: &'a [LocalFile], id: &'a str) -> &'a str {
        files
            .iter()
            .find(|file| file.id == id)
            .map_or(id, |file| file.path.as_str())
    }
    let path_of = |id: &'_ str| -> String { path_of(&files, id).to_owned() };
    let mut variants: Vec<DraftVariant> = Vec::new();
    let mut grouped: BTreeSet<String> = BTreeSet::new();
    let mut splits: BTreeMap<(String, u32), BTreeMap<u32, String>> = BTreeMap::new();
    for meta in &metas {
        let path = path_of(&meta.path);
        if let Some((prefix, index, total)) = quant::split_part(&path) {
            splits
                .entry((prefix.to_owned(), total))
                .or_default()
                .insert(index, meta.path.clone());
        }
    }
    for ((_, total), parts) in splits {
        let complete = parts.len() == total as usize && parts.keys().copied().eq(1..=total);
        let ids: Vec<String> = parts.into_values().collect();
        grouped.extend(ids.iter().cloned());
        let primary = ids[0].clone();
        variants.push((label_for(&path_of(&primary)), primary, ids, total, complete));
    }
    for meta in &metas {
        if grouped.contains(&meta.path) {
            continue;
        }
        let path = path_of(&meta.path);
        variants.push((
            label_for(&path),
            meta.path.clone(),
            vec![meta.path.clone()],
            1,
            true,
        ));
    }
    let file_by_id: BTreeMap<&str, &LocalFile> =
        files.iter().map(|file| (file.id.as_str(), file)).collect();
    let mut quants: Vec<LocalQuant> = variants
        .into_iter()
        .map(|(label, primary, mut ids, parts, complete)| {
            let set = sets.iter().find(|(_, members)| {
                members.first() == Some(&primary) || members.contains(&primary)
            });
            if let Some((_, members)) = set {
                for member in members {
                    if !ids.contains(member) && file_by_id.contains_key(member.as_str()) {
                        ids.push(member.clone());
                    }
                }
            }
            let members: Vec<&LocalFile> = ids
                .iter()
                .filter_map(|id| file_by_id.get(id.as_str()).copied())
                .collect();
            let first = file_by_id[primary.as_str()];
            let used_by: BTreeSet<String> = members
                .iter()
                .flat_map(|file| file.used_by.iter().cloned())
                .collect();
            let add = if !complete {
                None
            } else if let Some((set, _)) = set.filter(|_| first.storage == Storage::ArtifactStore) {
                Some(json!({"artifact_set": set}))
            } else if parts == 1 && first.storage == Storage::ArtifactStore {
                first
                    .artifact
                    .as_ref()
                    .map(|artifact| json!({"artifact": artifact}))
            } else if parts == 1 {
                Some(json!({"path": first.location}))
            } else {
                None
            };
            LocalQuant {
                label: label.unwrap_or_else(|| extension_label(&first.path)),
                file: primary.clone(),
                parts,
                complete,
                bytes: members.iter().map(|file| file.bytes).sum(),
                removable: members.iter().all(|file| file.removable)
                    && set.is_none_or(|(id, _)| !store_in_use.contains(id)),
                used_by: used_by.into_iter().collect(),
                files: ids,
                add,
            }
        })
        .collect();
    quants.sort_by(|left, right| {
        left.bytes
            .cmp(&right.bytes)
            .then(left.file.cmp(&right.file))
    });
    let used_by: BTreeSet<String> = files
        .iter()
        .flat_map(|file| file.used_by.iter().cloned())
        .collect();
    let mut storage: Vec<Storage> = Vec::new();
    for file in &files {
        if !storage.contains(&file.storage) {
            storage.push(file.storage);
        }
    }
    LocalGroup {
        id: match &repo {
            Some(repo) => hf_key(repo),
            None => files
                .first()
                .map_or_else(String::new, |file| format!("file:{}", file.id)),
        },
        repo,
        name,
        publisher,
        storage,
        location,
        read_only: files.iter().all(|file| file.read_only),
        bytes: files.iter().map(|file| file.bytes).sum(),
        removable_bytes: files
            .iter()
            .filter(|file| file.removable)
            .map(|file| file.bytes)
            .sum(),
        used_by: used_by.into_iter().collect(),
        quants,
        files,
    }
}

pub fn scan(inputs: &ScanInputs, parse: &ResolveUrl) -> LocalInventory {
    let mut drafts = Drafts::default();
    for hub in &inputs.hubs {
        scan_hub(hub, &inputs.roots, &mut drafts);
    }
    if let Some(store) = &inputs.store {
        scan_store(store, inputs.store_read_only, parse, &mut drafts);
        drafts.truncated |= store.truncated;
    }
    scan_registrations(&inputs.registrations, &mut drafts);

    let mut by_path: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut by_artifact: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let set_members: BTreeMap<&str, Vec<&str>> = inputs
        .store
        .iter()
        .flat_map(|store| &store.sets)
        .map(|set| {
            (
                set.id.as_str(),
                set.specification
                    .members
                    .iter()
                    .map(|member| member.artifact.as_str())
                    .collect(),
            )
        })
        .collect();
    for registration in &inputs.registrations {
        for path in &registration.paths {
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            by_path
                .entry(canonical)
                .or_default()
                .insert(registration.id.clone());
        }
        let mut artifacts: Vec<&str> = registration.artifact.as_deref().into_iter().collect();
        if let Some(set) = registration.artifact_set.as_deref() {
            artifacts.extend(set_members.get(set).into_iter().flatten().copied());
        }
        for artifact in artifacts {
            by_artifact
                .entry(artifact.to_owned())
                .or_default()
                .insert(registration.id.clone());
        }
    }
    let mut busy: BTreeSet<String> = BTreeSet::new();
    let mut store_in_use: BTreeSet<String> = BTreeSet::new();
    if let Some(store) = &inputs.store {
        for file in &store.files {
            let ids = by_artifact.entry(file.id.clone()).or_default();
            for reference in &file.references {
                if let Some(model) = reference.strip_prefix("model:") {
                    ids.insert(model.to_owned());
                }
            }
            if file.active_holds > 0 || (!file.references.is_empty() && ids.is_empty()) {
                busy.insert(file.id.clone());
            }
        }
        for set in store.sets.iter().filter(|set| set.in_use) {
            store_in_use.insert(set.id.clone());
            for member in &set.specification.members {
                let ids = by_artifact.entry(member.artifact.clone()).or_default();
                for reference in &set.references {
                    if let Some(model) = reference.strip_prefix("model:") {
                        ids.insert(model.to_owned());
                    }
                }
                busy.insert(member.artifact.clone());
            }
        }
    }
    let owned_hubs: Vec<&Hub> = inputs.hubs.iter().filter(|hub| hub.owned).collect();
    for draft in drafts.order.values_mut() {
        for file in &mut draft.files {
            let mut used: BTreeSet<String> =
                by_path.get(&file.canonical).cloned().unwrap_or_default();
            used.extend(by_path.get(&file.location).into_iter().flatten().cloned());
            if let Some(artifact) = &file.artifact {
                used.extend(by_artifact.get(artifact).into_iter().flatten().cloned());
            }
            file.used_by = used.into_iter().collect();
            file.refusal = if file.read_only {
                Some("read_only")
            } else if !file.used_by.is_empty() {
                Some("in_use")
            } else if file.storage == Storage::Local
                || (file.storage == Storage::HuggingfaceCache
                    && !owned_hubs
                        .iter()
                        .any(|hub| file.location.starts_with(&hub.path)))
            {
                Some("not_owned")
            } else if file
                .artifact
                .as_ref()
                .is_some_and(|artifact| busy.contains(artifact))
            {
                Some("busy")
            } else {
                None
            };
            file.removable = file.refusal.is_none();
        }
    }
    let mut groups: Vec<LocalGroup> = drafts
        .order
        .into_values()
        .filter(|draft| !draft.files.is_empty())
        .map(|draft| finish(draft, &store_in_use))
        .collect();
    groups.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then(left.id.cmp(&right.id))
    });
    LocalInventory {
        schema_version: 1,
        artifact_generation: inputs.store.as_ref().map(|store| store.generation),
        total_bytes: groups.iter().map(|group| group.bytes).sum(),
        removable_bytes: groups.iter().map(|group| group.removable_bytes).sum(),
        truncated: drafts.truncated,
        hubs: inputs
            .hubs
            .iter()
            .map(|hub| HubView {
                path: hub.path.clone(),
                owned: hub.owned,
                read_only: hub.read_only,
            })
            .collect(),
        groups,
    }
}

/// Removes one snapshot entry of an owned Hugging Face cache, and its blob when no other
/// snapshot of the repository still links to it.
pub fn remove_cache_file(file: &LocalFile, roots: &[PathBuf]) -> std::io::Result<()> {
    let repo = file
        .repo_directory
        .as_deref()
        .ok_or_else(|| std::io::Error::other("cache file without repository"))?;
    if !file.location.starts_with(repo.join("snapshots")) {
        return Err(std::io::Error::other("cache entry outside its snapshots"));
    }
    let entry = std::fs::symlink_metadata(&file.location)?;
    let blob = std::fs::canonicalize(&file.location)?;
    std::fs::remove_file(&file.location)?;
    if entry.file_type().is_symlink() && blob.starts_with(repo.join("blobs")) {
        let still_linked =
            crate::native_import::snapshots(repo, None)
                .into_iter()
                .any(|snapshot| {
                    crate::native_import::list_files(&repo.join("snapshots").join(snapshot), roots)
                        .iter()
                        .any(|(_, canonical, _)| *canonical == blob)
                });
        if !still_linked {
            std::fs::remove_file(&blob)?;
        }
    }
    let snapshots = repo.join("snapshots");
    let mut directory = file.location.parent();
    while let Some(current) = directory {
        if current == snapshots || !current.starts_with(&snapshots) {
            break;
        }
        if std::fs::remove_dir(current).is_err() {
            break;
        }
        directory = current.parent();
    }
    Ok(())
}
