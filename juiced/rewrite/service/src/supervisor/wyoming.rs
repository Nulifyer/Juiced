use super::*;

impl Manager {
    pub async fn wyoming_settings(&self) -> (crate::wyoming::Settings, u64) {
        let config = self.config.lock().await;
        (config.wyoming.clone(), config.generation)
    }

    pub async fn configure_wyoming(
        self: &Arc<Self>,
        settings: crate::wyoming::Settings,
        expected_generation: u64,
    ) -> Result<u64, ApiError> {
        let config = self.config.clone().lock_owned().await;
        if config.generation != expected_generation {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Configuration changed. Refresh before changing Wyoming settings.",
            ));
        }
        let mut next = config.clone();
        next.wyoming = settings;
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "Configuration generation cannot advance.",
            )
        })?;
        let generation = next.generation;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Wyoming settings committed but directory durability is unconfirmed.",
            ));
        }
        Ok(generation)
    }
}
