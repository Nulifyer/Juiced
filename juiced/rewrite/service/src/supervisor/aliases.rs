use super::{Manager, ModelView, Worker, publication};
use crate::{
    ApiError,
    config::{MAXIMUM_ALIASES, ModelAliases, Policy, valid_model_id},
};
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasChange {
    pub alias: String,
    #[serde(deserialize_with = "explicit_target")]
    pub target: Option<String>,
    pub expected_revision: u64,
}

fn explicit_target<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

fn invalid_alias() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_alias",
        "Alias names follow model ID rules: nonempty, at most 256 bytes and without control characters.",
    )
}

pub(super) fn alias_available(view: &ModelView) -> bool {
    view.policy != Policy::Disabled && view.downloaded
}

impl Manager {
    pub(super) fn alias_snapshot(&self) -> Arc<ModelAliases> {
        self.aliases
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(super) fn refresh_aliases(&self, aliases: &ModelAliases) {
        let mut current = self
            .aliases
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if **current != *aliases {
            *current = Arc::new(aliases.clone());
        }
    }

    pub(super) fn resolve_model(
        &self,
        requested: &str,
    ) -> Result<(Arc<Worker>, Option<Arc<str>>), ApiError> {
        match self.worker(requested) {
            Ok(worker) => Ok((worker, None)),
            Err(missing) => {
                let aliases = self.alias_snapshot();
                let Some(target) = aliases.entries.get(requested) else {
                    return Err(missing);
                };
                Ok((self.worker(target)?, Some(Arc::from(requested))))
            }
        }
    }

    pub fn alias_target(&self, alias: &str) -> Option<String> {
        self.alias_snapshot().entries.get(alias).cloned()
    }

    pub fn aliased_views(&self, catalog: &[ModelView]) -> Vec<(String, ModelView)> {
        self.alias_snapshot()
            .entries
            .iter()
            .filter_map(|(alias, target)| {
                catalog
                    .iter()
                    .find(|view| view.id == *target)
                    .map(|view| (alias.clone(), view.clone()))
            })
            .collect()
    }

    pub async fn aliases_view(&self) -> serde_json::Value {
        let catalog = self.catalog().await;
        let aliases = self.alias_snapshot();
        let entries: Vec<_> = aliases
            .entries
            .iter()
            .map(|(alias, target)| {
                let view = catalog.iter().find(|view| view.id == *target);
                serde_json::json!({
                    "alias": alias,
                    "target": target,
                    "target_state": view.map(|view| view.state),
                    "target_policy": view.map(|view| view.policy),
                    "available": view.is_some_and(alias_available),
                })
            })
            .collect();
        serde_json::json!({"revision": aliases.revision, "maximum": MAXIMUM_ALIASES, "aliases": entries})
    }

    pub async fn set_alias(
        self: &Arc<Self>,
        change: AliasChange,
    ) -> Result<serde_json::Value, ApiError> {
        if !valid_model_id(&change.alias)
            || change
                .target
                .as_deref()
                .is_some_and(|target| !valid_model_id(target))
        {
            return Err(invalid_alias());
        }
        let config = self.config.clone().lock_owned().await;
        if config.aliases.revision != change.expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The aliases changed in another client. Refresh before saving again.",
            ));
        }
        if config.models.iter().any(|model| model.id == change.alias) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "alias_conflict",
                "An alias cannot reuse a registered model ID.",
            ));
        }
        let mut next = config.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration generation cannot advance.",
            )
        })?;
        next.aliases.revision = next.generation;
        match change.target {
            Some(target) => {
                if !config.models.iter().any(|model| model.id == target) {
                    return Err(ApiError::new(
                        StatusCode::NOT_FOUND,
                        "unknown_model",
                        "The alias target is not a registered model.",
                    ));
                }
                if !config.aliases.entries.contains_key(&change.alias)
                    && config.aliases.entries.len() >= MAXIMUM_ALIASES
                {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "alias_limit",
                        format!("At most {MAXIMUM_ALIASES} aliases can be saved."),
                    ));
                }
                next.aliases.entries.insert(change.alias, target);
            }
            None => {
                if next.aliases.entries.remove(&change.alias).is_none() {
                    return Err(ApiError::new(
                        StatusCode::NOT_FOUND,
                        "unknown_alias",
                        "The alias is not saved.",
                    ));
                }
            }
        }
        next.validate().map_err(|_| invalid_alias())?;
        let next = Self::check_config_size(next).await?;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "The alias change committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        Ok(self.aliases_view().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::tests::manager;

    fn change(alias: &str, target: Option<&str>, expected_revision: u64) -> AliasChange {
        AliasChange {
            alias: alias.into(),
            target: target.map(str::to_owned),
            expected_revision,
        }
    }

    #[test]
    fn alias_changes_require_an_explicit_target_field() {
        assert!(
            serde_json::from_str::<AliasChange>(r#"{"alias":"chat","expected_revision":0}"#)
                .is_err()
        );
        let deletion: AliasChange =
            serde_json::from_str(r#"{"alias":"chat","target":null,"expected_revision":0}"#)
                .unwrap();
        assert!(deletion.target.is_none());
        assert!(
            serde_json::from_str::<AliasChange>(
                r#"{"alias":"chat","target":"one","expected_revision":0,"extra":1}"#
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn aliases_validate_resolve_persist_and_compare_revisions() {
        let (_directory, manager) = manager();
        let target = manager.worker_snapshot()[0].spec.id.clone();
        for invalid in ["", "   ", "line\nbreak", &"a".repeat(257)] {
            let error = manager
                .set_alias(change(invalid, Some(&target), 0))
                .await
                .unwrap_err();
            assert_eq!(error.code, "invalid_alias");
        }
        let error = manager
            .set_alias(change(&target, Some(&target), 0))
            .await
            .unwrap_err();
        assert_eq!(error.code, "alias_conflict");
        let error = manager
            .set_alias(change("chat", Some("absent"), 0))
            .await
            .unwrap_err();
        assert_eq!(error.code, "unknown_model");
        let error = manager
            .set_alias(change("chat", None, 0))
            .await
            .unwrap_err();
        assert_eq!(error.code, "unknown_alias");

        let saved = manager
            .set_alias(change("chat", Some(&target), 0))
            .await
            .unwrap();
        let revision = saved["revision"].as_u64().unwrap();
        assert_ne!(revision, 0);
        assert_eq!(saved["aliases"][0]["alias"], "chat");
        assert_eq!(saved["aliases"][0]["target"], target.as_str());
        let error = manager
            .set_alias(change("chat", None, 0))
            .await
            .unwrap_err();
        assert_eq!(error.code, "revision_conflict");

        let (worker, alias) = manager.resolve_model("chat").unwrap();
        assert_eq!(worker.spec.id, target);
        assert_eq!(alias.as_deref(), Some("chat"));
        let (worker, alias) = manager.resolve_model(&target).unwrap();
        assert_eq!(worker.spec.id, target);
        assert!(alias.is_none());
        assert_eq!(
            manager.resolve_model("absent").err().unwrap().code,
            "unknown_model"
        );

        let persisted = crate::config::Config::read(&manager.config_path).unwrap();
        assert_eq!(persisted.aliases.revision, revision);
        assert_eq!(persisted.aliases.entries.get("chat"), Some(&target));

        let disabled = manager
            .set_policy(&target, Policy::Disabled, 1)
            .await
            .unwrap();
        let error = manager
            .unregister(&target, disabled.revision)
            .await
            .unwrap_err();
        assert_eq!(error.code, "alias_target");
        assert!(manager.resolve_model("chat").is_ok());
        let disabled_alias = manager
            .begin_queued(
                "chat",
                crate::config::Operation::Completion,
                crate::admission::Priority::Interactive,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(disabled_alias.code, "model_disabled");

        let removed = manager
            .set_alias(change("chat", None, revision))
            .await
            .unwrap();
        assert!(removed["aliases"].as_array().unwrap().is_empty());
        assert!(manager.resolve_model("chat").is_err());
        let persisted = crate::config::Config::read(&manager.config_path).unwrap();
        assert!(persisted.aliases.entries.is_empty());
    }
}
