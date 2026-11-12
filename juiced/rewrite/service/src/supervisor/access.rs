use super::*;
use crate::protocol_access::{Gate, Protocol, Settings};

impl Manager {
    pub fn protocol_gate(&self) -> &Arc<Gate> {
        &self.protocol_gate
    }

    pub fn open_key_store(&self) -> Arc<crate::api_keys::KeyStore> {
        let directory = self.config_root.as_ref().map_or_else(
            || {
                self.config_path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .to_owned()
            },
            |root| root.root().to_owned(),
        );
        crate::api_keys::KeyStore::open(directory, self.config_root.clone())
    }

    pub async fn protocol_settings(&self) -> Settings {
        self.config.lock().await.protocols.clone()
    }

    pub async fn set_protocol_enabled(
        self: &Arc<Self>,
        protocol: Protocol,
        enabled: bool,
    ) -> Result<Settings, ApiError> {
        let config = self.config.clone().lock_owned().await;
        let mut next = config.clone();
        if enabled {
            next.protocols.disabled.remove(&protocol);
        } else {
            next.protocols.disabled.insert(protocol);
        }
        if next.protocols == config.protocols {
            return Ok(next.protocols);
        }
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
        let settings = self.protocol_settings().await;
        self.protocol_gate.apply(&settings);
        let outcome = outcome?;
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Protocol settings committed but directory durability is unconfirmed.",
            ));
        }
        Ok(settings)
    }
}
