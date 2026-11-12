use super::*;
use crate::{
    artifacts::{ArtifactStore, Intake},
    engines::{ChoiceError, models},
};
use axum::body::Body;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
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
}

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

/// Returns N for `name-0000k-of-0000N.gguf` with N > 1.
fn split_parts(name: &str) -> Option<u32> {
    let stem = name.to_ascii_lowercase();
    let stem = stem.strip_suffix(".gguf")?;
    let (rest, total) = stem.rsplit_once("-of-")?;
    let (_, part) = rest.rsplit_once('-')?;
    let digits = |text: &str| text.len() == 5 && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(part) || !digits(total) {
        return None;
    }
    total.parse().ok().filter(|total| *total > 1)
}

fn reject_split(name: &str) -> Result<(), ApiError> {
    if split_parts(name).is_some() {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "split_model_unsupported",
            "Split GGUF parts cannot be added as one model yet. Merge them with llama-gguf-split --merge and import the merged file.",
        ));
    }
    Ok(())
}

fn single_model(format: &models::ModelFormat) -> Result<(), ApiError> {
    if let models::ModelFormat::Gguf { declared, .. } = format
        && declared.architecture.as_deref() == Some("clip")
    {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "projector_unsupported",
            "Vision projector (mmproj) files are companions, and companion projectors cannot be added yet. Import the language model GGUF.",
        ));
    }
    Ok(())
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
            "Only files under the configured import_roots can be imported; links that resolve outside them are refused.",
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
    let path = canonical_within(roots, raw)?;
    let (file, bytes) = open_regular(&path)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            failure(
                StatusCode::BAD_REQUEST,
                "import_path_not_file",
                "Import one regular, nonempty model file.",
            )
        })?;
    reject_split(&name)?;
    let (format, _) = models::detect(&path).map_err(super::engines::format_error)?;
    single_model(&format)?;
    Ok((
        Linked {
            name,
            path,
            bytes,
            format,
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
        let installed = crate::engines::installed_packages(&packages);
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
        reject_split(&query.filename)?;
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
        let (published, format) = tokio::task::spawn_blocking(move || {
            let (format, _) =
                models::detect(&intake.staging_path()).map_err(super::engines::format_error)?;
            single_model(&format)?;
            let published = intake.finish().map_err(ApiError::from)?;
            Ok::<_, ApiError>((published, format))
        })
        .await
        .map_err(|_| interrupted())??;
        let suggested = models::default_id(&query.filename);
        Ok(json!({
            "schema_version": 1,
            "artifact": published.artifact,
            "job": published.job,
            "file": query.filename,
            "bytes": published.bytes,
            "sha256": published.artifact,
            "format": format,
            "duplicate": published.duplicate,
            "suggested_id": suggested,
            "engines": self.engine_options(&format).await,
            "add": {"artifact": published.artifact, "id": suggested},
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
                    "List one readable directory under the configured import_roots.",
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
                    "split": kind == "file" && split_parts(&name).is_some(),
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
        let (linked, job) = tokio::task::spawn_blocking(move || {
            let (linked, file) = inspect_file(&roots, &raw)?;
            let Some(store) = store else {
                return Ok((linked, None));
            };
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
            Ok::<_, ApiError>((linked, Some(job)))
        })
        .await
        .map_err(|_| interrupted())??;
        let suggested = models::default_id(&linked.name);
        let add = (mode == ImportMode::Link).then(|| json!({"path": linked.path, "id": suggested}));
        Ok(json!({
            "schema_version": 1,
            "mode": if mode == ImportMode::Link { "link" } else { "copy" },
            "job": job,
            "path": linked.path,
            "file": linked.name,
            "bytes": linked.bytes,
            "format": linked.format,
            "suggested_id": suggested,
            "engines": self.engine_options(&linked.format).await,
            "add": add,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_parts_and_names_are_recognized() {
        assert_eq!(split_parts("Qwen3-30B-Q4_K_M-00001-of-00003.gguf"), Some(3));
        assert_eq!(split_parts("model-00002-OF-00002.GGUF"), Some(2));
        assert_eq!(split_parts("single-00001-of-00001.gguf"), None);
        assert_eq!(split_parts("model-1-of-3.gguf"), None);
        assert_eq!(split_parts("plain.gguf"), None);
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
