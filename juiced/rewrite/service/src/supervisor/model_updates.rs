use super::Manager;
use crate::ApiError;
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    sync::Arc,
};

const MAXIMUM_REPOSITORIES: usize = 64;
const MAXIMUM_SOURCES: usize = 4096;

/// Where a registered model's main weights came from on the Hugging Face hub, with the local
/// file digest when it is known without reading the file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Origin {
    repo: String,
    file: String,
    revision: Option<String>,
    sha256: Option<String>,
}

fn digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Reads `…/models--ORG--NAME/snapshots/REVISION/FILE` from a Hugging Face cache path. Cache
/// snapshots link LFS files to `blobs/SHA256`, so the link names the digest.
fn cache_origin(path: &Path) -> Option<Origin> {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    let at = parts.iter().position(|part| part.starts_with("models--"))?;
    let (organization, name) = parts[at].strip_prefix("models--")?.split_once("--")?;
    if parts.get(at + 1) != Some(&"snapshots") || parts.len() < at + 4 {
        return None;
    }
    let sha256 = std::fs::read_link(path)
        .ok()
        .and_then(|target| target.file_name()?.to_str().map(str::to_owned))
        .filter(|name| digest(name));
    Some(Origin {
        repo: format!("{organization}/{name}"),
        file: parts[at + 3..].join("/"),
        revision: Some(parts[at + 2].to_owned()),
        sha256,
    })
}

enum Outcome {
    Current,
    Newer,
    Failed(&'static str),
}

fn compare(origin: &Origin, revision: Option<&str>, files: &[(String, Option<String>)]) -> Outcome {
    let Some((_, remote)) = files.iter().find(|(path, _)| *path == origin.file) else {
        return Outcome::Failed("file_not_upstream");
    };
    match (&origin.sha256, remote) {
        (Some(local), Some(remote)) => {
            if local.eq_ignore_ascii_case(remote) {
                Outcome::Current
            } else {
                Outcome::Newer
            }
        }
        _ => match (&origin.revision, revision) {
            (Some(local), Some(remote)) if local == remote => Outcome::Current,
            (Some(_), Some(_)) => Outcome::Newer,
            _ => Outcome::Failed("digest_unknown"),
        },
    }
}

impl Manager {
    /// Compares each downloaded model's main weights with the current upstream file, as native
    /// `POST /models/check-updates` did. Models from other sources are not checked.
    pub async fn check_model_updates(self: &Arc<Self>) -> Result<Value, ApiError> {
        let library = self.library().clone();
        if let Some(error) = library.hub().blocked() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "update_check_offline",
                format!(
                    "Cannot check model updates while the hub is unavailable ({}).",
                    error.code()
                ),
            ));
        }
        let parent = self
            .config_path
            .parent()
            .expect("configuration parent")
            .to_owned();
        let models: Vec<(String, Option<String>, std::path::PathBuf)> = {
            let config = self.config.lock().await;
            config
                .models
                .iter()
                .map(|model| {
                    let file = if model.model_file.is_relative() {
                        parent.join(&model.model_file)
                    } else {
                        model.model_file.clone()
                    };
                    (model.id.clone(), model.artifact.clone(), file)
                })
                .collect()
        };
        let prefix = library.hub().base_prefix();
        let sources = match (self.artifacts(), &prefix) {
            (Ok(store), Some(prefix)) => {
                let prefix = format!("{prefix}/");
                tokio::task::spawn_blocking(move || {
                    store
                        .completed_sources(&prefix, MAXIMUM_SOURCES)
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let hub = library.clone();
        let origins: Vec<(String, Option<Origin>)> = tokio::task::spawn_blocking(move || {
            models
                .into_iter()
                .map(|(id, artifact, file)| {
                    let managed = artifact.as_deref().and_then(|artifact| {
                        sources
                            .iter()
                            .find(|record| record.sha256 == artifact)
                            .and_then(|record| {
                                let (repo, revision, file) =
                                    hub.hub().parse_resolve_url(&record.source_url)?;
                                Some(Origin {
                                    repo,
                                    file,
                                    revision: Some(revision),
                                    sha256: Some(record.sha256.clone()),
                                })
                            })
                    });
                    (id, managed.or_else(|| cache_origin(&file)))
                })
                .collect()
        })
        .await
        .unwrap_or_default();
        let mut repositories: Vec<String> = origins
            .iter()
            .filter_map(|(_, origin)| origin.as_ref().map(|origin| origin.repo.clone()))
            .collect();
        repositories.sort();
        repositories.dedup();
        let mut upstream = BTreeMap::new();
        for repo in repositories.iter().take(MAXIMUM_REPOSITORIES) {
            let result = library
                .hub()
                .info(repo, None, true)
                .await
                .map(|(info, _)| {
                    (
                        info.revision.clone(),
                        info.files
                            .iter()
                            .map(|file| (file.path.clone(), file.sha256.clone()))
                            .collect::<Vec<_>>(),
                    )
                })
                .map_err(|error| error.code());
            upstream.insert(repo.clone(), result);
        }
        let mut updated = Vec::new();
        let mut failed = serde_json::Map::new();
        let mut determined = BTreeMap::new();
        let mut checked = 0usize;
        for (id, origin) in origins {
            let Some(origin) = origin else {
                continue;
            };
            checked += 1;
            let outcome = match upstream.get(&origin.repo) {
                None => Outcome::Failed("repository_limit"),
                Some(Err(code)) => Outcome::Failed(code),
                Some(Ok((revision, files))) => compare(&origin, revision.as_deref(), files),
            };
            match outcome {
                Outcome::Current => {
                    determined.insert(id, false);
                }
                Outcome::Newer => {
                    determined.insert(id.clone(), true);
                    updated.push(id);
                }
                Outcome::Failed(code) => {
                    failed.insert(id, json!(code));
                }
            }
        }
        library.record_updates(determined);
        Ok(json!({
            "status": if failed.is_empty() { "success" } else { "failed" },
            "updates_available": updated.len(),
            "models": updated,
            "failed_models": failed,
            "checked": checked,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_name_repository_revision_file_and_linked_digest() {
        let directory = tempfile::tempdir().unwrap();
        let blob = "a".repeat(64);
        let root = directory.path().join("hub/models--acme--Tiny-GGUF");
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::create_dir_all(root.join("snapshots/0123abc/sub")).unwrap();
        std::fs::write(root.join("blobs").join(&blob), b"weights").unwrap();
        let file = root.join("snapshots/0123abc/sub/tiny-Q4_0.gguf");
        #[cfg(unix)]
        std::os::unix::fs::symlink(format!("../../../blobs/{blob}"), &file).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&file, b"weights").unwrap();
        let origin = cache_origin(&file).unwrap();
        assert_eq!(origin.repo, "acme/Tiny-GGUF");
        assert_eq!(origin.file, "sub/tiny-Q4_0.gguf");
        assert_eq!(origin.revision.as_deref(), Some("0123abc"));
        #[cfg(unix)]
        assert_eq!(origin.sha256.as_deref(), Some(blob.as_str()));
        assert!(cache_origin(Path::new("/models/plain.gguf")).is_none());
    }

    #[test]
    fn digests_decide_before_revisions() {
        let origin = Origin {
            repo: "acme/x".into(),
            file: "x.gguf".into(),
            revision: Some("old".into()),
            sha256: Some("b".repeat(64)),
        };
        let same = vec![("x.gguf".to_owned(), Some("b".repeat(64)))];
        let changed = vec![("x.gguf".to_owned(), Some("c".repeat(64)))];
        assert!(matches!(
            compare(&origin, Some("new"), &same),
            Outcome::Current
        ));
        assert!(matches!(
            compare(&origin, Some("old"), &changed),
            Outcome::Newer
        ));
        assert!(matches!(
            compare(&origin, Some("new"), &[]),
            Outcome::Failed("file_not_upstream")
        ));
        let copied = Origin {
            sha256: None,
            ..origin
        };
        assert!(matches!(
            compare(&copied, Some("new"), &same),
            Outcome::Newer
        ));
        assert!(matches!(
            compare(&copied, Some("old"), &same),
            Outcome::Current
        ));
    }
}
