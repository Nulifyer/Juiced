use super::*;
use crate::library::{LocalInventory, LocalInventoryFile, LocalStorage};
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRemoval {
    pub model: String,
    pub expected_revision: u64,
}

struct Kept<'a> {
    file: &'a LocalInventoryFile,
    reason: &'static str,
    others: Vec<String>,
}

struct Plan<'a> {
    deletes: Vec<&'a LocalInventoryFile>,
    keeps: Vec<Kept<'a>>,
    refusal: Option<(&'static str, String)>,
}

fn folder(file: &LocalInventoryFile) -> String {
    file.location
        .parent()
        .unwrap_or(&file.location)
        .display()
        .to_string()
}

/// What removing `id` does to each file it uses. Files another model uses stay, and so does
/// a file linked from outside Juiced's storage, which nothing would find again. A file Juiced
/// would find again but cannot delete refuses the removal: the model would come straight back.
fn plan<'a>(inventory: &'a LocalInventory, id: &str) -> Plan<'a> {
    let mut plan = Plan {
        deletes: Vec::new(),
        keeps: Vec::new(),
        refusal: None,
    };
    let owned: Vec<&std::path::Path> = inventory
        .hubs
        .iter()
        .filter(|hub| hub.owned)
        .map(|hub| hub.path.as_path())
        .collect();
    for file in inventory.groups.iter().flat_map(|group| &group.files) {
        if !file.used_by.iter().any(|user| user == id) {
            continue;
        }
        let others: Vec<String> = file
            .used_by
            .iter()
            .filter(|user| *user != id)
            .cloned()
            .collect();
        if !others.is_empty() {
            plan.keeps.push(Kept {
                file,
                reason: "shared",
                others,
            });
            continue;
        }
        if file.storage == LocalStorage::Local {
            plan.keeps.push(Kept {
                file,
                reason: "linked",
                others,
            });
            continue;
        }
        let refusal = if file.read_only {
            Some((
                "model_files_read_only",
                format!(
                    "{} is in a read-only folder ({}), so Juiced can't delete it. Set the model to Disabled to keep it out of use.",
                    file.name,
                    folder(file)
                ),
            ))
        } else if file.storage == LocalStorage::HuggingfaceCache
            && !owned.iter().any(|hub| file.location.starts_with(hub))
        {
            Some((
                "model_files_not_owned",
                format!(
                    "{} is in a Hugging Face cache outside Juiced's data folder ({}), so Juiced won't delete it. Delete it there, or set the model to Disabled.",
                    file.name,
                    folder(file)
                ),
            ))
        } else {
            None
        };
        plan.refusal = plan.refusal.or(refusal);
        plan.deletes.push(file);
    }
    plan
}

impl Manager {
    async fn removal_state(
        &self,
        id: &str,
    ) -> Result<(ModelView, Option<(&'static str, String)>), ApiError> {
        self.worker(id)?;
        let view = self
            .catalog()
            .await
            .into_iter()
            .find(|model| model.id == id)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "model_not_found",
                    "No model has this ID.",
                )
            })?;
        let aliases: Vec<String> = self
            .config
            .lock()
            .await
            .aliases
            .entries
            .iter()
            .filter(|(_, target)| *target == id)
            .map(|(alias, _)| alias.clone())
            .take(8)
            .collect();
        let refusal = if view.active_requests > 0 {
            Some((
                "model_in_use",
                "Requests are still running on this model. Wait for them to finish.".to_owned(),
            ))
        } else if view.loaded || view.pid.is_some() || view.preparation_pending {
            Some(("model_in_use", "Unload the model first.".to_owned()))
        } else if !aliases.is_empty() {
            Some((
                "alias_target",
                format!(
                    "Re-point or delete the aliases that target this model first: {}.",
                    aliases.join(", ")
                ),
            ))
        } else {
            None
        };
        Ok((view, refusal))
    }

    /// What removing a model deletes, what stays and why, and the reason when it can't be
    /// removed.
    pub async fn removal_plan(&self, id: &str) -> Result<Value, ApiError> {
        let (view, state) = self.removal_state(id).await?;
        let inventory = self.local_library(false).await?;
        let plan = plan(&inventory, id);
        let refusal = state.or(plan.refusal);
        Ok(json!({
            "schema_version": 1,
            "model": id,
            "revision": view.revision,
            "removable": refusal.is_none(),
            "refusal": refusal.map(|(code, message)| json!({"code": code, "message": message})),
            "freed_bytes": plan.deletes.iter().map(|file| file.bytes).sum::<u64>(),
            "deletes": plan.deletes.iter().map(|file| json!({
                "name": file.name, "bytes": file.bytes, "location": file.location,
            })).collect::<Vec<_>>(),
            "keeps": plan.keeps.iter().map(|kept| json!({
                "name": kept.file.name, "bytes": kept.file.bytes, "location": kept.file.location,
                "reason": kept.reason, "used_by": kept.others,
            })).collect::<Vec<_>>(),
        }))
    }

    /// Removes a model and deletes the files only it uses. Nothing changes when a file can't
    /// be deleted or the model is loaded or busy.
    pub async fn remove_model(self: &Arc<Self>, request: ModelRemoval) -> Result<Value, ApiError> {
        let id = request.model.as_str();
        let (view, state) = self.removal_state(id).await?;
        if view.revision != request.expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The model changed. Refresh before removing it.",
            ));
        }
        let inventory = self.local_library(true).await?;
        let (deletes, kept) = {
            let plan = plan(&inventory, id);
            if let Some((code, message)) = state.or(plan.refusal) {
                return Err(ApiError::new(StatusCode::CONFLICT, code, message));
            }
            (
                plan.deletes
                    .iter()
                    .map(|file| (file.id.clone(), file.name.clone()))
                    .collect::<Vec<_>>(),
                plan.keeps
                    .iter()
                    .map(|kept| json!({"name": kept.file.name, "reason": kept.reason, "used_by": kept.others}))
                    .collect::<Vec<_>>(),
            )
        };
        let mut revision = view.revision;
        if view.policy != Policy::Disabled {
            revision = self
                .set_policy(id, Policy::Disabled, revision)
                .await?
                .revision;
        }
        let removal = self.unregister(id, revision).await?;
        let ids: Vec<String> = deletes.iter().map(|(id, _)| id.clone()).collect();
        let (deleted, freed, failed) = match self.delete_unused(&ids).await {
            Ok(result) => result,
            Err(_) => (Vec::new(), 0, ids.clone()),
        };
        let name = |id: &String| {
            deletes
                .iter()
                .find(|(known, _)| known == id)
                .map_or_else(|| id.clone(), |(_, name)| name.clone())
        };
        let mut kept = kept;
        kept.extend(
            failed
                .iter()
                .map(|id| json!({"name": name(id), "reason": "undeleted", "used_by": []})),
        );
        self.register_local_models();
        Ok(json!({
            "schema_version": 1,
            "model": id,
            "removed": true,
            "freed_bytes": freed,
            "deleted": deleted.iter().map(name).collect::<Vec<_>>(),
            "kept": kept,
            "sessions_forgotten": removal["sessions_forgotten"],
        }))
    }
}
