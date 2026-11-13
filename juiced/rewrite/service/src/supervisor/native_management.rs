use super::*;
use crate::artifacts::ArtifactRole;
use serde_json::{Value, json};

/// What the native management adapters need to know about one registration.
#[derive(Clone, Debug)]
pub struct NativeModel {
    pub view: ModelView,
    pub artifact: Option<String>,
    pub artifact_set: Option<String>,
    pub previous_artifact_set: Option<String>,
    pub model_file: std::path::PathBuf,
    pub companions: Vec<(String, ArtifactRole, std::path::PathBuf)>,
    pub recipe: Option<&'static str>,
}

/// Disk released after a registration was removed.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReleasedWeights {
    pub removed_artifacts: usize,
    pub freed_bytes: u64,
    pub retained_artifacts: usize,
    pub unmanaged_files: usize,
}

fn native_role(role: ArtifactRole) -> String {
    match role {
        ArtifactRole::Model => "main".into(),
        ArtifactRole::Projector => "mmproj".into(),
        ArtifactRole::DraftModel => "draft".into(),
        ArtifactRole::Encoder => "text_encoder".into(),
        ArtifactRole::Vae => "vae".into(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "other".into()),
    }
}

fn file_entry(name: String, role: String, path: &std::path::Path, include_path: bool) -> Value {
    let metadata = std::fs::metadata(path).ok();
    let mut entry = json!({
        "name": name,
        "role": role,
        "size_bytes": metadata.as_ref().map_or(0, |metadata| metadata.len()),
        "exists": metadata.is_some(),
    });
    if include_path {
        entry["path"] = json!(path);
    }
    entry
}

impl Manager {
    pub async fn native_model(&self, id: &str) -> Option<NativeModel> {
        let view = self
            .catalog()
            .await
            .into_iter()
            .find(|model| model.id == id)?;
        let worker = self.worker(id).ok()?;
        let spec = &worker.spec;
        Some(NativeModel {
            view,
            artifact: spec.artifact.clone(),
            artifact_set: spec.artifact_set.clone(),
            previous_artifact_set: spec
                .previous_artifact_set
                .as_ref()
                .map(|binding| binding.artifact_set.clone()),
            model_file: spec.model_file.clone(),
            companions: spec
                .companions
                .iter()
                .map(|companion| {
                    (
                        companion.name.clone(),
                        companion.role,
                        companion.path.clone(),
                    )
                })
                .collect(),
            recipe: discovery::recipe(spec.execution_profile.adapter),
        })
    }

    /// Per-model file inventory in the native `GET /models/{id}/files` shape.
    pub async fn native_files(&self, id: &str, include_paths: bool) -> Result<Value, ApiError> {
        let model = self.native_model(id).await.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("Model '{id}' was not found."),
            )
        })?;
        let mut files = Vec::new();
        match (&model.artifact, &model.artifact_set, self.artifacts.clone()) {
            (Some(artifact), set, Some(store)) => {
                let members = match set {
                    Some(set) => {
                        let view = store.artifact_set(set).map_err(ApiError::from)?;
                        view.specification
                            .members
                            .iter()
                            .map(|member| {
                                (member.name.clone(), member.role, member.artifact.clone())
                            })
                            .collect()
                    }
                    None => vec![(
                        store.source_file_name(artifact).unwrap_or_else(|| {
                            model
                                .model_file
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| artifact.clone())
                        }),
                        ArtifactRole::Model,
                        artifact.clone(),
                    )],
                };
                for (name, role, artifact) in members {
                    if let Some(path) = store.stored_path(&artifact) {
                        files.push(file_entry(name, native_role(role), &path, include_paths));
                    }
                }
            }
            _ => {
                let name = model
                    .model_file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                files.push(file_entry(
                    name,
                    "main".into(),
                    &model.model_file,
                    include_paths,
                ));
                for (name, role, path) in &model.companions {
                    files.push(file_entry(
                        name.clone(),
                        native_role(*role),
                        path,
                        include_paths,
                    ));
                }
            }
        }
        Ok(json!({"model_id": id, "files": files}))
    }

    /// Removes the managed artifacts a removed registration used, unless something else still
    /// references them. Files outside the artifact store are never deleted here.
    pub async fn release_native_weights(&self, model: &NativeModel) -> ReleasedWeights {
        let Some(store) = self.artifacts.clone() else {
            return ReleasedWeights {
                unmanaged_files: 1 + model.companions.len(),
                ..ReleasedWeights::default()
            };
        };
        let Some(artifact) = model.artifact.clone() else {
            return ReleasedWeights {
                unmanaged_files: 1 + model.companions.len(),
                ..ReleasedWeights::default()
            };
        };
        let sets: Vec<String> = [
            model.artifact_set.clone(),
            model.previous_artifact_set.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        tokio::task::spawn_blocking(move || {
            let mut released = ReleasedWeights::default();
            let mut artifacts = vec![artifact];
            for set in sets {
                let Ok(view) = store.artifact_set(&set) else {
                    continue;
                };
                artifacts.extend(
                    view.specification
                        .members
                        .iter()
                        .map(|member| member.artifact.clone()),
                );
                if view.references.is_empty() {
                    let _ = store.remove_set(&set, store.generation());
                }
            }
            artifacts.sort();
            artifacts.dedup();
            for artifact in artifacts {
                let bytes = store
                    .stored_path(&artifact)
                    .and_then(|path| std::fs::metadata(path).ok())
                    .map_or(0, |metadata| metadata.len());
                match store.remove(&artifact, store.generation()) {
                    Ok(()) => {
                        released.removed_artifacts += 1;
                        released.freed_bytes += bytes;
                    }
                    Err(crate::artifacts::ArtifactError::NotFound) => {}
                    Err(_) => released.retained_artifacts += 1,
                }
            }
            released
        })
        .await
        .unwrap_or_default()
    }
}
