use super::*;
use crate::barista::{Change, Settings};

impl Manager {
    pub async fn barista_settings(&self) -> Settings {
        self.config.lock().await.barista.clone()
    }

    pub async fn configure_barista(self: &Arc<Self>, change: Change) -> Result<Settings, ApiError> {
        let model = change.model.filter(|model| !model.is_empty());
        if let Some(id) = model.as_deref()
            && !crate::config::valid_model_id(id)
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_assistant_model",
                "Name one registered chat model.",
            ));
        }
        if change.enabled {
            let Some(id) = model.as_deref() else {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "assistant_model_required",
                    "Choose a chat model before turning Barista on.",
                ));
            };
            let catalog = self.catalog().await;
            let Some(view) = catalog.iter().find(|view| view.id == id) else {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "assistant_model_not_found",
                    "That model is not registered.",
                ));
            };
            if !view.operations.contains(&Operation::Chat) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "assistant_model_not_chat",
                    "Barista needs a model that serves chat completions.",
                ));
            }
            if !view.downloaded {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "assistant_model_not_downloaded",
                    "Download the model before choosing it for Barista.",
                ));
            }
        }
        let config = self.config.clone().lock_owned().await;
        if config.barista.revision != change.expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Barista settings changed. Refresh before changing them.",
            ));
        }
        if change.enabled {
            let spec = model
                .as_deref()
                .and_then(|id| config.models.iter().find(|spec| spec.id == id));
            match spec {
                None => {
                    return Err(ApiError::new(
                        StatusCode::NOT_FOUND,
                        "assistant_model_not_found",
                        "That model is not registered.",
                    ));
                }
                Some(spec) if spec.policy == Policy::Disabled => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "assistant_model_disabled",
                        "The model is Disabled. Set it to On demand or Hot first.",
                    ));
                }
                Some(_) => {}
            }
        }
        let mut next = config.clone();
        next.barista.enabled = change.enabled;
        next.barista.model = model;
        next.barista.permission = change.permission;
        if next.barista == config.barista {
            return Ok(next.barista);
        }
        let exhausted = || {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "Configuration generation cannot advance.",
            )
        };
        next.barista.revision = next.barista.revision.checked_add(1).ok_or_else(exhausted)?;
        next.generation = next.generation.checked_add(1).ok_or_else(exhausted)?;
        let settings = next.barista.clone();
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Barista settings committed but directory durability is unconfirmed.",
            ));
        }
        Ok(settings)
    }
}
