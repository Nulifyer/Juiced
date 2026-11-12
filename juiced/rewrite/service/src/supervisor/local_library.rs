use super::*;
use crate::library::{
    LOCAL_STORE_LISTING, LocalHub, LocalInventory, LocalStorage, Registered, ScanInputs,
};
use serde_json::{Value, json};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::Path,
};

const MAXIMUM_DELETE: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDelete {
    pub files: Vec<String>,
}

fn failure(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(status, code, message)
}

fn interrupted() -> ApiError {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "local_delete_interrupted",
        "Deleting the files was interrupted. Refresh the list before retrying.",
    )
}

fn registrations(config: &Config, parent: &Path) -> Vec<Registered> {
    config
        .resolved_models(parent)
        .into_iter()
        .map(|spec| {
            let mut paths = Vec::new();
            if spec.artifact.is_none() {
                paths.push(spec.model_file.clone());
                paths.extend(
                    spec.companions
                        .iter()
                        .map(|companion| companion.path.clone()),
                );
            }
            Registered {
                id: spec.id,
                paths,
                artifact: spec.artifact,
                artifact_set: spec.artifact_set,
            }
        })
        .collect()
}

/// Hugging Face hub directories to list: the native search order plus the cache locations the
/// `huggingface_hub` library would use for this process, when they lie under a readable root.
fn hubs(config_path: &Path, roots: &[PathBuf]) -> Vec<LocalHub> {
    let owned = config_path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok());
    let mut found =
        crate::native_import::weights_roots(config_path, None, roots).unwrap_or_default();
    let environment = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let extra = [
        environment("HF_HUB_CACHE"),
        environment("HF_HOME").map(|home| home.join("hub")),
        environment("XDG_CACHE_HOME").map(|cache| cache.join("huggingface").join("hub")),
    ];
    for candidate in extra.into_iter().flatten() {
        if let Ok(path) = std::fs::canonicalize(&candidate)
            && path.is_dir()
            && roots.iter().any(|root| path.starts_with(root))
            && !found.contains(&path)
        {
            found.push(path);
        }
    }
    found
        .into_iter()
        .map(|path| LocalHub {
            owned: owned.as_ref().is_some_and(|owned| path.starts_with(owned)),
            read_only: !crate::library::writable(&path),
            path,
        })
        .collect()
}

fn fingerprint(registrations: &[Registered], generation: Option<u64>, roots: &[PathBuf]) -> u64 {
    let mut hasher = DefaultHasher::new();
    generation.hash(&mut hasher);
    roots.hash(&mut hasher);
    for registration in registrations {
        registration.id.hash(&mut hasher);
        registration.paths.hash(&mut hasher);
        registration.artifact.hash(&mut hasher);
        registration.artifact_set.hash(&mut hasher);
    }
    hasher.finish()
}

impl Manager {
    async fn scan_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.canonical_import_roots().await;
        if let Some(owned) = self
            .config_path
            .parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            && !roots.iter().any(|root| owned.starts_with(root))
        {
            roots.push(owned);
        }
        roots
    }

    async fn local_with(
        &self,
        config: &Config,
        roots: Vec<PathBuf>,
        fresh: bool,
    ) -> Result<Arc<LocalInventory>, ApiError> {
        let parent = self
            .config_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_owned();
        let registrations = registrations(config, &parent);
        let store = self.artifacts.clone();
        let key = fingerprint(
            &registrations,
            store.as_ref().map(|store| store.generation()),
            &roots,
        );
        let config_path = self.config_path.clone();
        self.library
            .local_inventory(key, fresh, move || ScanInputs {
                hubs: hubs(&config_path, &roots),
                store: store.and_then(|store| store.local_listing(LOCAL_STORE_LISTING).ok()),
                store_read_only: false,
                registrations,
                roots,
            })
            .await
    }

    pub async fn local_library(&self, fresh: bool) -> Result<Arc<LocalInventory>, ApiError> {
        let roots = self.scan_roots().await;
        let config = self.config.lock().await.clone();
        self.local_with(&config, roots, fresh).await
    }

    pub async fn delete_local_files(&self, request: LocalDelete) -> Result<Value, ApiError> {
        if request.files.is_empty() || request.files.len() > MAXIMUM_DELETE {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "invalid_local_delete",
                format!("Name between 1 and {MAXIMUM_DELETE} files from the local inventory."),
            ));
        }
        let roots = self.scan_roots().await;
        // The configuration lock keeps a registration from claiming a file between the
        // in-use check and its removal.
        let config = self.config.lock().await;
        let inventory = self.local_with(&config, roots.clone(), true).await?;
        let mut files = Vec::with_capacity(request.files.len());
        for id in &request.files {
            let file = inventory.file(id).ok_or_else(|| {
                failure(
                    StatusCode::NOT_FOUND,
                    "local_file_not_found",
                    "This file is no longer on disk. Refresh the list.",
                )
            })?;
            match file.refusal {
                None => {}
                Some("read_only") => {
                    return Err(failure(
                        StatusCode::CONFLICT,
                        "local_file_read_only",
                        format!(
                            "{} is in a read-only folder, so Juiced can't delete it.",
                            file.name
                        ),
                    ));
                }
                Some("in_use") => {
                    return Err(failure(
                        StatusCode::CONFLICT,
                        "local_file_in_use",
                        format!(
                            "{} is used by {}. Remove those models first.",
                            file.name,
                            file.used_by.join(", ")
                        ),
                    ));
                }
                Some("not_owned") => {
                    return Err(failure(
                        StatusCode::FORBIDDEN,
                        "local_file_not_owned",
                        format!(
                            "{} is outside Juiced's own data folder, so Juiced won't delete it.",
                            file.name
                        ),
                    ));
                }
                Some(_) => {
                    return Err(failure(
                        StatusCode::CONFLICT,
                        "local_file_busy",
                        format!(
                            "{} is held by a running worker or download. Retry when it is idle.",
                            file.name
                        ),
                    ));
                }
            }
            if !files
                .iter()
                .any(|known: &crate::library::LocalInventoryFile| known.id == file.id)
            {
                files.push(file.clone());
            }
        }
        let store = self.artifacts.clone();
        let deleted = tokio::task::spawn_blocking(move || -> Result<Value, ApiError> {
            let mut freed = 0u64;
            let mut deleted = Vec::new();
            let artifacts: Vec<&str> = files
                .iter()
                .filter_map(|file| file.artifact.as_deref())
                .collect();
            if !artifacts.is_empty() {
                let store = store.as_ref().ok_or_else(|| {
                    failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "artifact_store_unavailable",
                        "Managed artifact storage is unavailable.",
                    )
                })?;
                let listing = store
                    .local_listing(LOCAL_STORE_LISTING)
                    .map_err(ApiError::from)?;
                for set in listing.sets.iter().filter(|set| {
                    set.specification
                        .members
                        .iter()
                        .any(|member| artifacts.contains(&member.artifact.as_str()))
                }) {
                    store
                        .remove_set(&set.id, store.generation())
                        .map_err(ApiError::from)?;
                }
            }
            for file in &files {
                match (file.storage, &file.artifact, &store) {
                    (LocalStorage::ArtifactStore, Some(artifact), Some(store)) => {
                        store
                            .remove(artifact, store.generation())
                            .map_err(ApiError::from)?;
                    }
                    (LocalStorage::HuggingfaceCache, _, _) => {
                        crate::library::remove_cache_file(file, &roots).map_err(|_| {
                            failure(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "local_delete_failed",
                                format!(
                                    "{} couldn't be deleted. Check the folder's permissions.",
                                    file.name
                                ),
                            )
                        })?;
                    }
                    _ => return Err(interrupted()),
                }
                freed = freed.saturating_add(file.bytes);
                deleted.push(file.id.clone());
            }
            Ok(json!({"schema_version": 1, "deleted": deleted, "freed_bytes": freed}))
        })
        .await
        .map_err(|_| interrupted());
        self.library.invalidate_local();
        drop(config);
        deleted?
    }
}
