use super::*;

impl Manager {
    pub fn release_notice(&self) -> serde_json::Value {
        self.releases.projection(crate::VERSION)
    }

    pub fn start_release_check(&self) -> Result<tokio::task::JoinHandle<()>, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()?;
        let releases = self.releases.clone();
        Ok(tokio::spawn(async move { releases.run(client).await }))
    }

    pub async fn set_release_check(self: &Arc<Self>, enabled: bool) -> Result<(), ApiError> {
        let config = self.config.clone().lock_owned().await;
        if config.release_check == enabled {
            self.releases.set_enabled(enabled);
            return Ok(());
        }
        let mut next = config.clone();
        next.release_check = enabled;
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "Configuration generation cannot advance.",
            )
        })?;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await;
        self.releases
            .set_enabled(self.config.lock().await.release_check);
        if outcome?.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "The release check setting committed but directory durability is unconfirmed.",
            ));
        }
        Ok(())
    }
}
