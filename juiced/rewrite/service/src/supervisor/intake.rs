use super::{companion_sets, *};
use crate::{
    artifacts::{ArtifactStore, Intake},
    engines::{ChoiceError, models, split},
};
use axum::body::Body;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

const UPLOAD_BUFFER: usize = 4 * 1024 * 1024;
const UPLOAD_IDLE: Duration = Duration::from_secs(120);
const FREE_SPACE_MARGIN: u64 = 64 * 1024 * 1024;
const NAME_BYTES: usize = 255;
const PATH_BYTES: usize = 4096;
const MAXIMUM_ENTRIES: usize = 1000;
const CANDIDATE_EXTENSIONS: &[&str] = &["gguf", "bin", "safetensors", "onnx"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadQuery {
    pub filename: String,
    pub size: u64,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ImportMode {
    Copy,
    Link,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPathRequest {
    pub path: String,
    pub mode: ImportMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportListQuery {
    pub dir: Option<String>,
}

pub(super) struct Linked {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub format: models::ModelFormat,
    /// Every part of a split GGUF in order, the first included; empty for one file.
    pub parts: Vec<Part>,
}

pub(super) struct Part {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
}

impl Linked {
    fn role(&self) -> &'static str {
        if models::is_projector(&self.format) {
            "projector"
        } else {
            "model"
        }
    }
}

const MAXIMUM_PROJECTORS: usize = 8;

fn failure(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(status, code, message)
}

fn interrupted() -> ApiError {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "import_interrupted",
        "The local import was interrupted. Nothing was kept.",
    )
}

fn valid_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty()
        || name.len() > NAME_BYTES
        || name.trim() != name
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "invalid_upload_name",
            "Name the uploaded file with one bounded file name without path separators.",
        ));
    }
    Ok(())
}

/// Checks the header of a file named or declared as a split part; returns `(part, parts)`.
fn split_header(name: &str, file: &mut File) -> Result<Option<(u32, u32)>, ApiError> {
    match split::parse(name) {
        Some((_, part, parts)) => {
            if parts > split::MAXIMUM_PARTS {
                return Err(companion_sets::parts_limit());
            }
            split::verify(file, part, parts)
                .map_err(|_| companion_sets::part_mismatch(name, part, parts))?;
            Ok(Some((part, parts)))
        }
        None => match split::declared_parts(file) {
            Ok(Some(_)) => Err(companion_sets::name_invalid()),
            _ => Ok(None),
        },
    }
}

/// Every part of the split model `name` belongs to, read from `directory` under the names
/// llama.cpp derives; each must stay inside the roots and agree with its header.
fn split_group(
    roots: &[PathBuf],
    directory: &Path,
    name: &str,
) -> Result<Option<(Vec<Part>, File)>, ApiError> {
    let Some((prefix, _, parts)) = split::parse(name) else {
        return Ok(None);
    };
    if parts > split::MAXIMUM_PARTS {
        return Err(companion_sets::parts_limit());
    }
    let mut group = Vec::with_capacity(parts as usize);
    let mut first = None;
    for part in 1..=parts {
        let name = split::name(prefix, part, parts);
        let path = directory.join(&name);
        let canonical = match fs::canonicalize(&path) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(companion_sets::part_missing(&name, part, parts));
            }
            Err(_) => {
                return Err(failure(
                    StatusCode::FORBIDDEN,
                    "import_path_unreadable",
                    "The service cannot read this import path.",
                ));
            }
        };
        if !contained(roots, &canonical) {
            return Err(failure(
                StatusCode::FORBIDDEN,
                "import_path_outside_roots",
                "This folder isn't one Juiced may import from.",
            ));
        }
        let (mut file, bytes) = open_regular(&canonical)?;
        split::verify(&mut file, part, parts)
            .map_err(|_| companion_sets::part_mismatch(&name, part, parts))?;
        if part == 1 {
            first = Some(file);
        }
        group.push(Part { name, path, bytes });
    }
    Ok(first.map(|file| (group, file)))
}

/// The first part of the only split model in `directory`.
fn split_in_directory(directory: &Path) -> Option<String> {
    let mut found = None;
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if split::parse(&name).is_some_and(|(_, part, _)| part == 1) {
            if found.is_some() {
                return None;
            }
            found = Some(name);
        }
    }
    found
}

/// Vision projectors beside a model, offered for its `projector`.
fn projector_candidates(roots: &[PathBuf], directory: &Path) -> Vec<Value> {
    let Ok(reader) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut names: Vec<String> = reader
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            !name.starts_with('.') && lower.ends_with(".gguf") && lower.contains("mmproj")
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let path = directory.join(&name);
            let canonical = fs::canonicalize(&path).ok()?;
            if !contained(roots, &canonical) {
                return None;
            }
            let (_, bytes) = open_regular(&canonical).ok()?;
            let (format, _) = models::detect(&canonical).ok()?;
            models::is_projector(&format)
                .then(|| json!({"name": name, "path": path, "bytes": bytes}))
        })
        .take(MAXIMUM_PROJECTORS)
        .collect()
}

/// A Hugging Face cache snapshot is named by its revision, so the repository name two
/// levels up (`models--org--name/snapshots/<revision>`) names the model instead.
fn directory_name(directory: &Path) -> Option<String> {
    let name = directory.file_name()?.to_string_lossy().into_owned();
    let repository = directory
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "snapshots"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("models--"))
        .and_then(|name| name.rsplit("--").next())
        .map(str::to_owned);
    Some(repository.unwrap_or(name))
}

fn free_space(store: &ArtifactStore, bytes: u64) -> Result<(), ApiError> {
    if store
        .available_bytes()
        .is_some_and(|available| available < bytes.saturating_add(FREE_SPACE_MARGIN))
    {
        return Err(failure(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            "The artifact storage volume does not have enough free space for this file.",
        ));
    }
    Ok(())
}

fn modified_ms(metadata: &fs::Metadata) -> Option<u64> {
    let elapsed = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}

fn contained(roots: &[PathBuf], path: &Path) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn requested_path(raw: &str) -> Result<PathBuf, ApiError> {
    let path = Path::new(raw);
    if raw.is_empty()
        || raw.len() > PATH_BYTES
        || raw.chars().any(char::is_control)
        || !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "invalid_import_path",
            "Name one absolute server path of at most 4096 bytes without parent references.",
        ));
    }
    Ok(path.to_owned())
}

fn canonical_within(roots: &[PathBuf], raw: &str) -> Result<PathBuf, ApiError> {
    let canonical = fs::canonicalize(requested_path(raw)?).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            failure(
                StatusCode::NOT_FOUND,
                "import_path_not_found",
                "No file or directory exists at this import path.",
            )
        } else {
            failure(
                StatusCode::FORBIDDEN,
                "import_path_unreadable",
                "The service cannot read this import path.",
            )
        }
    })?;
    if !contained(roots, &canonical) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "import_path_outside_roots",
            "This folder isn't one Juiced may import from.",
        ));
    }
    Ok(canonical)
}

fn open_regular(path: &Path) -> Result<(File, u64), ApiError> {
    let not_file = || {
        failure(
            StatusCode::BAD_REQUEST,
            "import_path_not_file",
            "Import one regular, nonempty model file.",
        )
    };
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| not_file())?;
    let metadata = file.metadata().map_err(|_| not_file())?;
    if !metadata.is_file()
        || metadata.len() == 0
        || !crate::storage::same_file(&file, path).unwrap_or(false)
    {
        return Err(not_file());
    }
    Ok((file, metadata.len()))
}

fn inspect_file(roots: &[PathBuf], raw: &str) -> Result<(Linked, File), ApiError> {
    let requested = requested_path(raw)?;
    let mut path = canonical_within(roots, raw)?;
    // Checkpoint directories and split GGUF parts register by a path beside their siblings.
    // In a Hugging Face snapshot that path links into `blobs/`, so the opened file is the
    // link target while the registered path stays in the snapshot.
    let mut anchor = None;
    if path.is_dir() {
        if let Some(found) = ["model.onnx", "config.json"]
            .into_iter()
            .map(|name| path.join(name))
            .find(|anchor| anchor.is_file())
        {
            path = canonical_within(roots, &found.to_string_lossy())?;
            anchor = Some(found);
        } else if let Some(first) = split_in_directory(&path) {
            anchor = Some(path.join(first));
        }
    } else if let (Some(name), Some(parent)) = (
        requested.file_name().and_then(|name| name.to_str()),
        requested.parent(),
    ) && split::parse(name).is_some()
    {
        anchor = Some(canonical_within(roots, &parent.to_string_lossy())?.join(name));
    }
    let not_file = || {
        failure(
            StatusCode::BAD_REQUEST,
            "import_path_not_file",
            "Import one regular, nonempty model file.",
        )
    };
    let registered = anchor.clone().unwrap_or_else(|| path.clone());
    let name = registered
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(not_file)?;
    let directory = registered.parent().ok_or_else(not_file)?;
    let (mut file, mut bytes, path, name, parts) = match split_group(roots, directory, &name)? {
        Some((parts, file)) => (
            file,
            parts.iter().map(|part| part.bytes).sum(),
            parts[0].path.clone(),
            parts[0].name.clone(),
            parts,
        ),
        None => {
            let (mut file, bytes) = open_regular(&path)?;
            if let Ok(Some(_)) = split::declared_parts(&mut file) {
                return Err(companion_sets::name_invalid());
            }
            (file, bytes, registered, name, Vec::new())
        }
    };
    file.seek(SeekFrom::Start(0)).map_err(|_| not_file())?;
    let (format, detected) = models::detect(&path).map_err(super::engines::format_error)?;
    if !parts.is_empty() && models::is_projector(&format) {
        return Err(companion_sets::projector_invalid());
    }
    let name = match &format {
        models::ModelFormat::Transformers { .. } | models::ModelFormat::OnnxClassifier { .. } => {
            bytes = detected.max(bytes);
            path.parent().and_then(directory_name).unwrap_or(name)
        }
        _ => name,
    };
    Ok((
        Linked {
            name,
            path,
            bytes,
            format,
            parts,
        },
        file,
    ))
}

async fn write_buffer(mut intake: Intake, buffer: Vec<u8>) -> Result<(Intake, Vec<u8>), ApiError> {
    let (intake, mut buffer, result) = tokio::task::spawn_blocking(move || {
        let result = intake.write(&buffer);
        (intake, buffer, result)
    })
    .await
    .map_err(|_| interrupted())?;
    result.map_err(ApiError::from)?;
    buffer.clear();
    Ok((intake, buffer))
}

impl Manager {
    /// Configured roots plus the read roots the operator granted for native adoption.
    async fn configured_import_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.config.lock().await.import_roots.clone();
        for root in super::native_adoption::from_env()
            .map(|(_, roots)| roots)
            .unwrap_or_default()
        {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    pub(super) async fn canonical_import_roots(&self) -> Vec<PathBuf> {
        let configured = self.configured_import_roots().await;
        tokio::task::spawn_blocking(move || {
            configured
                .iter()
                .filter_map(|root| fs::canonicalize(root).ok())
                .filter(|root| root.is_dir())
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    async fn engine_options(&self, format: &models::ModelFormat) -> Value {
        let engine = format.engine();
        let row = |ready: bool, error: Option<&str>| {
            json!({
                "engine": engine.name(), "recipe": engine.recipe(), "ready": ready,
                "backend": null, "package": null, "gpu": null, "reason": null,
                "error": error, "install": null, "installed_backends": [],
            })
        };
        let (snapshot, inventory) = match self.host().await {
            Ok(host) => host,
            Err(error) => return json!([row(false, Some(error.code))]),
        };
        let packages = match self.package_inventory().await {
            Ok(packages) => packages,
            Err(error) => return json!([row(false, Some(error.code))]),
        };
        let mut installed = crate::engines::installed_packages(&packages);
        installed.extend(Self::environment_packages(
            &self.environment_records().await,
        ));
        let mut value = row(false, None);
        let mut backends: Vec<&str> = installed
            .iter()
            .filter(|package| {
                package.installed && package.declared_host_match && package.engine == Some(engine)
            })
            .filter_map(|package| package.backend.as_deref())
            .collect();
        backends.sort_unstable();
        backends.dedup();
        value["installed_backends"] = json!(backends);
        match crate::engines::choose(engine, &inventory, &snapshot.report, &installed, None) {
            Ok(choice) => {
                value["ready"] = json!(true);
                value["backend"] = json!(choice.backend);
                value["package"] = json!(choice.package);
                value["gpu"] = json!(choice.gpu);
                value["reason"] = json!(choice.reason);
            }
            Err(ChoiceError::NotInstalled {
                backend,
                gpu_target,
                installable,
            }) => {
                value["backend"] = json!(backend);
                value["error"] = json!("engine_not_installed");
                value["install"] = json!({
                    "engine": engine.name(), "backend": backend, "gpu_target": gpu_target,
                    "installable": installable, "expected_generation": packages["generation"],
                });
            }
            Err(ChoiceError::NoUsableBackend) => value["error"] = json!("no_usable_backend"),
            Err(ChoiceError::NotSelectable(_)) => value["error"] = json!("backend_not_selectable"),
        }
        json!([value])
    }

    pub async fn library_upload(
        self: &Arc<Self>,
        query: UploadQuery,
        content_length: Option<u64>,
        body: Body,
    ) -> Result<Value, ApiError> {
        valid_name(&query.filename)?;
        let maximum = self.config.lock().await.upload_max_bytes;
        if query.size > maximum {
            return Err(failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload_too_large",
                format!("Uploads are limited to {maximum} bytes by upload_max_bytes."),
            ));
        }
        if query.size == 0 || content_length.is_some_and(|length| length != query.size) {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "upload_size_mismatch",
                "Declare the exact nonzero file size with size and send exactly that many bytes.",
            ));
        }
        if split::parse(&query.filename).is_some_and(|(_, _, parts)| parts > split::MAXIMUM_PARTS) {
            return Err(companion_sets::parts_limit());
        }
        let store = self.artifacts()?;
        free_space(&store, query.size)?;
        let name = query.filename.clone();
        let size = query.size;
        let opener = store.clone();
        let mut intake = tokio::task::spawn_blocking(move || opener.begin_intake(name, size))
            .await
            .map_err(|_| interrupted())?
            .map_err(ApiError::from)?;
        let mut stream = body.into_data_stream();
        let mut buffer = Vec::with_capacity(UPLOAD_BUFFER.min(size as usize));
        let mut received = 0u64;
        let mut screened = false;
        loop {
            let next = tokio::select! {
                biased;
                _ = intake.stopped() => {
                    return Err(failure(
                        StatusCode::CONFLICT,
                        "upload_cancelled",
                        "The upload was cancelled or the service is stopping. Nothing was kept.",
                    ));
                }
                next = tokio::time::timeout(UPLOAD_IDLE, stream.next()) => next,
            };
            let chunk = match next {
                Err(_) => {
                    return Err(failure(
                        StatusCode::REQUEST_TIMEOUT,
                        "upload_stalled",
                        "The upload sent no bytes for two minutes. Nothing was kept.",
                    ));
                }
                Ok(None) => break,
                Ok(Some(Err(_))) => {
                    return Err(failure(
                        StatusCode::BAD_REQUEST,
                        "upload_incomplete",
                        "The upload stream failed before it completed. Nothing was kept.",
                    ));
                }
                Ok(Some(Ok(chunk))) => chunk,
            };
            received = received.saturating_add(chunk.len() as u64);
            if received > size {
                return Err(failure(
                    StatusCode::BAD_REQUEST,
                    "upload_size_mismatch",
                    "The upload sent more bytes than its declared size. Nothing was kept.",
                ));
            }
            buffer.extend_from_slice(&chunk);
            if !screened && (buffer.len() >= 8 || received == size) {
                screened = true;
                let prefix: Option<[u8; 8]> =
                    buffer.get(..8).and_then(|bytes| bytes.try_into().ok());
                if !prefix.is_some_and(|prefix| models::plausible_prefix(&prefix, size)) {
                    return Err(super::engines::format_error(
                        models::FormatError::Unsupported,
                    ));
                }
            }
            if buffer.len() >= UPLOAD_BUFFER {
                (intake, buffer) = write_buffer(intake, buffer).await?;
            }
        }
        if received != size {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "upload_incomplete",
                "The upload ended before its declared size. Nothing was kept.",
            ));
        }
        if !buffer.is_empty() {
            (intake, _) = write_buffer(intake, buffer).await?;
        }
        let filename = query.filename.clone();
        let (published, format, part) = tokio::task::spawn_blocking(move || {
            let staging = intake.staging_path();
            let (format, _) = models::detect(&staging).map_err(super::engines::format_error)?;
            let part = match &format {
                models::ModelFormat::Gguf { .. } => {
                    let mut file = File::open(&staging).map_err(|_| interrupted())?;
                    split_header(&filename, &mut file)?
                }
                _ => None,
            };
            if part.is_some() && models::is_projector(&format) {
                return Err(companion_sets::projector_invalid());
            }
            let published = intake.finish().map_err(ApiError::from)?;
            Ok::<_, ApiError>((published, format, part))
        })
        .await
        .map_err(|_| interrupted())??;
        let suggested = models::default_id(&query.filename);
        let projector = models::is_projector(&format);
        let add = (!projector && part.is_none())
            .then(|| json!({"artifact": published.artifact, "id": suggested}));
        let engines = if projector {
            json!([])
        } else {
            self.engine_options(&format).await
        };
        Ok(json!({
            "schema_version": 1,
            "artifact": published.artifact,
            "job": published.job,
            "file": query.filename,
            "bytes": published.bytes,
            "sha256": published.artifact,
            "format": format,
            "role": if projector { "projector" } else { "model" },
            "split": part.map(|(part, parts)| json!({"part": part, "parts": parts})),
            "duplicate": published.duplicate,
            "suggested_id": suggested,
            "engines": engines,
            "add": add,
            "artifact_generation": store.generation(),
        }))
    }

    pub async fn import_candidates(&self, query: ImportListQuery) -> Result<Value, ApiError> {
        let configured = self.configured_import_roots().await;
        let roots = self.canonical_import_roots().await;
        let root_views: Vec<Value> = configured
            .iter()
            .map(|root| {
                let resolved = fs::canonicalize(root).ok().filter(|path| path.is_dir());
                json!({"path": root, "resolved": resolved, "available": resolved.is_some()})
            })
            .collect();
        if configured.is_empty() {
            return Ok(json!({
                "schema_version": 1, "enabled": false, "roots": root_views,
                "directory": null, "parent": null, "entries": [], "truncated": false,
            }));
        }
        let Some(dir) = query.dir else {
            let entries: Vec<Value> = roots
                .iter()
                .map(|root| {
                    json!({
                        "name": root.to_string_lossy(), "path": root, "kind": "directory",
                        "bytes": null,
                        "modified_unix_ms": fs::metadata(root).ok().as_ref().and_then(modified_ms),
                    })
                })
                .collect();
            return Ok(json!({
                "schema_version": 1, "enabled": true, "roots": root_views,
                "directory": null, "parent": null, "entries": entries, "truncated": false,
            }));
        };
        let listed = tokio::task::spawn_blocking(move || -> Result<Value, ApiError> {
            let directory = canonical_within(&roots, &dir)?;
            let reader = fs::read_dir(&directory).map_err(|_| {
                failure(
                    StatusCode::BAD_REQUEST,
                    "import_path_not_directory",
                    "Choose a readable folder Juiced may import from.",
                )
            })?;
            let mut entries = Vec::new();
            let mut truncated = false;
            for entry in reader.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(resolved) = fs::canonicalize(entry.path()) else {
                    continue;
                };
                if !contained(&roots, &resolved) {
                    continue;
                }
                let Ok(metadata) = fs::metadata(&resolved) else {
                    continue;
                };
                let kind = if metadata.is_dir() {
                    "directory"
                } else if metadata.is_file()
                    && Path::new(&name)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            CANDIDATE_EXTENSIONS
                                .iter()
                                .any(|known| extension.eq_ignore_ascii_case(known))
                        })
                {
                    "file"
                } else {
                    continue;
                };
                if entries.len() == MAXIMUM_ENTRIES {
                    truncated = true;
                    break;
                }
                entries.push(json!({
                    "name": name, "path": entry.path(), "kind": kind,
                    "bytes": (kind == "file").then_some(metadata.len()),
                    "modified_unix_ms": modified_ms(&metadata),
                    "split": kind == "file" && split::parse(&name).is_some(),
                }));
            }
            entries.sort_by(|left, right| {
                (left["kind"] != "directory", left["name"].as_str())
                    .cmp(&(right["kind"] != "directory", right["name"].as_str()))
            });
            let parent = directory
                .parent()
                .filter(|parent| contained(&roots, parent))
                .map(Path::to_owned);
            Ok(json!({
                "directory": directory, "parent": parent, "entries": entries, "truncated": truncated,
            }))
        })
        .await
        .map_err(|_| interrupted())??;
        Ok(json!({
            "schema_version": 1, "enabled": true, "roots": root_views,
            "directory": listed["directory"], "parent": listed["parent"],
            "entries": listed["entries"], "truncated": listed["truncated"],
        }))
    }

    async fn import_roots_required(&self) -> Result<Vec<PathBuf>, ApiError> {
        let roots = self.canonical_import_roots().await;
        if roots.is_empty() {
            return Err(failure(
                StatusCode::FORBIDDEN,
                "import_roots_unconfigured",
                "Server-side import is disabled. Configure import_roots with directories the service may read.",
            ));
        }
        Ok(roots)
    }

    pub(super) async fn linked_import(&self, raw: &str) -> Result<Linked, ApiError> {
        let roots = self.import_roots_required().await?;
        let raw = raw.to_owned();
        tokio::task::spawn_blocking(move || inspect_file(&roots, &raw).map(|(linked, _)| linked))
            .await
            .map_err(|_| interrupted())?
    }

    /// Reads a file the local scan listed; those lie under the import roots or Juiced's own
    /// data folder.
    pub(super) async fn scanned_import(&self, raw: &str) -> Result<Linked, ApiError> {
        let roots = self.scan_roots().await;
        let raw = raw.to_owned();
        tokio::task::spawn_blocking(move || inspect_file(&roots, &raw).map(|(linked, _)| linked))
            .await
            .map_err(|_| interrupted())?
    }

    pub async fn import_path(
        self: &Arc<Self>,
        request: ImportPathRequest,
    ) -> Result<Value, ApiError> {
        let roots = self.import_roots_required().await?;
        let maximum = self.config.lock().await.upload_max_bytes;
        let store = if request.mode == ImportMode::Copy {
            Some(self.artifacts()?)
        } else {
            None
        };
        let raw = request.path.clone();
        let mode = request.mode;
        let (linked, job, projectors) = tokio::task::spawn_blocking(move || {
            let (linked, file) = inspect_file(&roots, &raw)?;
            let projectors = match (linked.role(), &linked.format, linked.path.parent()) {
                ("model", models::ModelFormat::Gguf { .. }, Some(directory)) => {
                    projector_candidates(&roots, directory)
                }
                _ => Vec::new(),
            };
            let Some(store) = store else {
                return Ok((linked, None, projectors));
            };
            if matches!(linked.format, models::ModelFormat::Transformers { .. }) {
                return Err(failure(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "checkpoint_copy_unsupported",
                    "Transformers checkpoint directories are registered in place. Import with mode link.",
                ));
            }
            if !linked.parts.is_empty() {
                return Err(failure(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "split_copy_unsupported",
                    "Split GGUF models are registered in place. Import with mode link.",
                ));
            }
            if linked.bytes > maximum {
                return Err(failure(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "upload_too_large",
                    format!("Imports are limited to {maximum} bytes by upload_max_bytes."),
                ));
            }
            free_space(&store, linked.bytes)?;
            let job = store
                .start_copy(file, linked.name.clone())
                .map_err(ApiError::from)?;
            Ok::<_, ApiError>((linked, Some(job), projectors))
        })
        .await
        .map_err(|_| interrupted())??;
        let suggested = models::default_id(&linked.name);
        let role = linked.role();
        let add = (mode == ImportMode::Link && role == "model")
            .then(|| json!({"path": linked.path, "id": suggested}));
        let engines = if role == "model" {
            self.engine_options(&linked.format).await
        } else {
            json!([])
        };
        let parts: Vec<Value> = linked
            .parts
            .iter()
            .map(|part| json!({"name": part.name, "path": part.path, "bytes": part.bytes}))
            .collect();
        Ok(json!({
            "schema_version": 1,
            "mode": if mode == ImportMode::Link { "link" } else { "copy" },
            "job": job,
            "path": linked.path,
            "file": linked.name,
            "bytes": linked.bytes,
            "format": linked.format,
            "role": role,
            "parts": parts,
            "projectors": projectors,
            "suggested_id": suggested,
            "engines": engines,
            "add": add,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_are_single_bounded_file_names() {
        for name in [
            "",
            ".",
            "..",
            "a/b.gguf",
            "a\\b.gguf",
            " lead.gguf",
            "nul\u{0}.gguf",
        ] {
            assert!(valid_name(name).is_err(), "{name:?}");
        }
        assert!(valid_name("Stories 15M.Q4_0.gguf").is_ok());
        assert!(valid_name(&"x".repeat(256)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn import_paths_stay_inside_roots_after_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("import");
        let outside = directory.path().join("outside");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("nested/inside.gguf"), b"GGUF").unwrap();
        fs::write(outside.join("secret.gguf"), b"GGUF").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.gguf"), root.join("escape.gguf")).unwrap();
        std::os::unix::fs::symlink(root.join("nested/inside.gguf"), root.join("alias.gguf"))
            .unwrap();
        let roots = vec![fs::canonicalize(&root).unwrap()];
        let text = |path: PathBuf| path.to_string_lossy().into_owned();
        assert!(canonical_within(&roots, &text(root.join("nested/inside.gguf"))).is_ok());
        assert!(canonical_within(&roots, &text(root.join("alias.gguf"))).is_ok());
        for refused in [root.join("escape.gguf"), outside.join("secret.gguf")] {
            assert_eq!(
                canonical_within(&roots, &text(refused)).unwrap_err().code,
                "import_path_outside_roots"
            );
        }
        assert_eq!(
            canonical_within(&roots, &text(root.join("nested/../escape.gguf")))
                .unwrap_err()
                .code,
            "invalid_import_path"
        );
        assert_eq!(
            canonical_within(&roots, "relative.gguf").unwrap_err().code,
            "invalid_import_path"
        );
        assert_eq!(
            open_regular(&roots[0]).unwrap_err().code,
            "import_path_not_file"
        );
    }
}
