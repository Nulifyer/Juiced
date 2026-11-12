use super::*;
use serde_json::{Value, json};

const MAX_VOICE_MODELS: usize = 64;

impl Manager {
    /// Voices come from each speech model's retained voice-pack companion, so listing them
    /// never starts a worker.
    pub async fn voices(&self, model: Option<&str>) -> Result<Vec<Value>, ApiError> {
        let workers = match model {
            Some(id) => {
                let worker = self.worker(id)?;
                if worker.spec.execution_profile.adapter != crate::config::Provider::Kokoros {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "voices_unavailable",
                        "This model is not a speech model with a retained voice pack.",
                    ));
                }
                vec![worker]
            }
            None => self
                .worker_snapshot()
                .into_iter()
                .filter(|worker| {
                    worker.spec.execution_profile.adapter == crate::config::Provider::Kokoros
                })
                .take(MAX_VOICE_MODELS)
                .collect(),
        };
        if workers.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.artifacts().ok();
        let requests: Vec<(String, Option<String>, Option<String>, Option<PathBuf>)> = workers
            .iter()
            .map(|worker| {
                (
                    worker.spec.id.clone(),
                    worker.spec.artifact.clone(),
                    worker.spec.artifact_set.clone(),
                    worker
                        .spec
                        .companions
                        .iter()
                        .find(|companion| companion.role == crate::artifacts::ArtifactRole::Voice)
                        .map(|companion| companion.path.clone()),
                )
            })
            .collect();
        tokio::task::spawn_blocking(move || {
            requests
                .into_iter()
                .map(|(id, artifact, set, in_place)| {
                    if let Some(path) = in_place {
                        let listed = crate::engines::models::voices(&path).map_err(|error| match error {
                            crate::engines::models::FormatError::Invalid(code) => code,
                            crate::engines::models::FormatError::Unavailable => "voice_pack_missing",
                            _ => "invalid_voice_pack",
                        });
                        return match listed {
                            Ok(voices) => json!({"model": id, "voices": voices, "error": null}),
                            Err(code) => json!({"model": id, "voices": [], "error": code}),
                        };
                    }
                    let listed = artifact
                        .ok_or("voice_pack_unmanaged")
                        .and_then(|artifact| {
                            let store = store.as_ref().ok_or("voice_pack_unmanaged")?;
                            store
                                .hold_binding_with(&artifact, set.as_deref(), true)
                                .map_err(|error| error.code())
                        })
                        .and_then(|lease| {
                            let (_, _, path) = lease
                                .companions()
                                .into_iter()
                                .find(|(_, role, _)| *role == crate::artifacts::ArtifactRole::Voice)
                                .ok_or("voice_pack_missing")?;
                            crate::engines::models::voices(path).map_err(|error| match error {
                                crate::engines::models::FormatError::Invalid(code) => code,
                                _ => "invalid_voice_pack",
                            })
                        });
                    match listed {
                        Ok(voices) => json!({"model": id, "voices": voices, "error": null}),
                        Err(code) => json!({"model": id, "voices": [], "error": code}),
                    }
                })
                .collect()
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "voice_listing_interrupted",
                "The voice listing was interrupted.",
            )
        })
    }
}
