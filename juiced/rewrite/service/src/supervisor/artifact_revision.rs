use super::*;
use crate::artifacts::ArtifactBinding;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ArtifactRevisionChange {
    Promote { artifact_set: String },
    Rollback,
}

fn current(spec: &ModelSpec) -> Option<ArtifactBinding> {
    Some(ArtifactBinding {
        artifact: spec.artifact.clone()?,
        artifact_set: spec.artifact_set.clone()?,
    })
}

impl Manager {
    pub async fn artifact_revision(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        let worker = self.worker(id)?;
        let state = worker.state.lock().await;
        self.ensure_registered(&worker)?;
        let supported = current(&worker.spec).is_some()
            && worker.spec.execution_profile.native_model_id().is_none()
            && worker.spec.resource_profile.is_none();
        let in_use = state.running.is_some()
            || worker.preparing.load(Ordering::Acquire)
            || worker.active.load(Ordering::Acquire) != 0
            || self.processes.snapshot(id).is_some();
        Ok(serde_json::json!({
            "schema_version":1,"model":id,"revision":state.revision,"policy":state.policy,
            "current":current(&worker.spec),"previous":worker.spec.previous_artifact_set,
            "supported":supported,"worker_in_use":in_use,"read_only":true,
            "compatibility":"unqualified","retention":"one previous complete set; weights are retained separately"
        }))
    }

    pub async fn set_artifact_revision(
        self: &Arc<Self>,
        id: String,
        expected_revision: u64,
        change: ArtifactRevisionChange,
    ) -> Result<serde_json::Value, ApiError> {
        let mut activity = self.publications.begin()?;
        activity.manager = Some(Arc::downgrade(self));
        let manager = self.clone();
        tokio::spawn(async move {
            let result = manager
                .change_artifact_revision(&id, expected_revision, change)
                .await;
            activity.finish();
            result
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "artifact_revision_unsettled",
                "Model revision publication did not settle. Inspect saved state before retrying.",
            )
        })?
    }

    async fn change_artifact_revision(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
        change: ArtifactRevisionChange,
    ) -> Result<serde_json::Value, ApiError> {
        let worker = self.worker(id)?;
        let state = worker.state.clone().lock_owned().await;
        self.ensure_registered(&worker)?;
        if state.revision != expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The model changed. Refresh before changing its artifact revision.",
            ));
        }
        let old = current(&worker.spec).filter(|_| worker.spec.execution_profile.native_model_id().is_none() && worker.spec.resource_profile.is_none())
            .ok_or_else(|| ApiError::new(StatusCode::NOT_IMPLEMENTED,"artifact_revision_unsupported",
                "This operation requires a complete managed set with local profile authority and no protected resource binding."))?;
        if state.running.is_some()
            || worker.preparing.load(Ordering::Acquire)
            || worker.active.load(Ordering::Acquire) != 0
            || self.processes.snapshot(id).is_some()
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_in_use",
                "Unload this worker before changing its artifact revision. Saved policy is preserved.",
            ));
        }
        let store = self.artifacts()?;
        let target = match change {
            ArtifactRevisionChange::Promote { artifact_set } => {
                let record = store.artifact_set(&artifact_set).map_err(ApiError::from)?;
                let primary = record
                    .specification
                    .members
                    .iter()
                    .find(|member| member.name == record.specification.primary)
                    .expect("validated primary");
                ArtifactBinding {
                    artifact: primary.artifact.clone(),
                    artifact_set,
                }
            }
            ArtifactRevisionChange::Rollback => worker
                .spec
                .previous_artifact_set
                .as_ref()
                .map(|previous| previous.as_ref().clone())
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "artifact_rollback_unavailable",
                        "No previous complete artifact set is retained for this model.",
                    )
                })?,
        };
        if target == old {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "artifact_revision_unchanged",
                "This artifact set is already selected.",
            ));
        }
        let held = {
            let store = store.clone();
            let binding = target.clone();
            tokio::task::spawn_blocking(move || {
                store.hold_binding(&binding.artifact, Some(&binding.artifact_set))
            })
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "artifact_inspection_interrupted",
                    "Artifact validation was interrupted.",
                )
            })?
            .map_err(ApiError::from)?
        };
        let config = self.config.clone().lock_owned().await;
        let mut next = config.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration generation cannot advance.",
            )
        })?;
        let spec = next
            .models
            .iter_mut()
            .find(|spec| spec.id == id)
            .expect("registered model");
        spec.artifact = Some(target.artifact.clone());
        spec.artifact_set = Some(target.artifact_set.clone());
        spec.previous_artifact_set = Some(Arc::new(old.clone()));
        spec.model_file = held.path().to_owned();
        spec.policy = state.policy;
        spec.revision = next.generation;
        let resolved = spec.resolved(self.config_path.parent().expect("configuration parent"));
        next.validate().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_artifact_revision",
                "The artifact revision cannot form a valid saved model registration.",
            )
        })?;
        let next = Self::check_config_size(next).await?;
        let resolved = self.bind_runtime(resolved).await?;
        let replacement = Self::create_worker(
            resolved,
            Some(Arc::new(held)),
            &self.admission,
            &self.processes,
            Some(worker.slots.clone()),
        )?;
        let reference = format!("model:{id}");
        let mut staged = vec![old.clone(), target.clone()];
        if let Some(previous) = &worker.spec.previous_artifact_set {
            staged.push(previous.as_ref().clone());
        }
        let prepared_store = store.clone();
        let prepared_reference = reference.clone();
        let (config, state, replacement) = tokio::task::spawn_blocking(move || {
            prepared_store.reference_bindings(&prepared_reference, &staged, prepared_store.generation()).map_err(ApiError::from)?;
            Ok::<_,ApiError>((config,state,replacement))
        }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR,"artifact_reference_interrupted","Artifact references were interrupted. Existing references are retained for inspection."))??;
        // References precede the configuration head, so an interrupted commit cannot expose unpinned inputs.
        let outcome = self
            .publish_configuration(
                config,
                next,
                publication::Effect::Replace {
                    previous: worker,
                    state,
                    replacement,
                },
            )
            .await?;
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Model revision committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        let configuration = self.config.clone().lock_owned().await;
        let retained = if configuration.generation != outcome.generation {
            drop(configuration);
            true
        } else {
            tokio::task::spawn_blocking(move || {
                let _configuration = configuration;
                store
                    .reference_bindings(&reference, &[target, old], store.generation())
                    .is_err()
            })
            .await
            .unwrap_or(true)
        };
        let mut view = self.artifact_revision(id).await?;
        view["obsolete_reference_retained"] = serde_json::json!(retained);
        Ok(view)
    }
}
