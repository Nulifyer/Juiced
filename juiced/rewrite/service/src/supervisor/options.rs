use super::{Manager, Worker, WorkerState, publication};
use crate::{
    ApiError,
    config::{Policy, Provider},
    execution_profile::ExecutionProfile,
    profiles::LaunchOptions,
};
use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::OwnedMutexGuard;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OptionsApply {
    #[default]
    NextLoad,
    Reload,
}

#[derive(Debug)]
pub enum OptionsError {
    Api(ApiError),
    ReloadFailed {
        status: StatusCode,
        cause: &'static str,
        restored_revision: Option<u64>,
        restarted: bool,
    },
}

impl From<ApiError> for OptionsError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl OptionsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Api(error) => error.code,
            Self::ReloadFailed { .. } => "options_reload_failed",
        }
    }
}

impl IntoResponse for OptionsError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::ReloadFailed {
                status,
                cause,
                restored_revision,
                restarted,
            } => {
                let message = match (restored_revision, restarted) {
                    (Some(_), true) => {
                        "The worker did not start with the new options. The previous options were restored and the worker restarted with them."
                    }
                    (Some(_), false) => {
                        "The worker did not start with the new options. The previous options were restored, but the worker did not restart. Inspect the worker before loading it again."
                    }
                    (None, _) => {
                        "The worker did not start with the new options, and the previous options could not be restored. Refresh and inspect the saved options."
                    }
                };
                (
                    status,
                    Json(serde_json::json!({"error": {
                        "code": "options_reload_failed",
                        "type": "juiced_error",
                        "message": message,
                        "cause": cause,
                        "rolled_back": restored_revision.is_some(),
                        "restored_revision": restored_revision,
                        "restarted": restarted,
                    }})),
                )
                    .into_response()
            }
        }
    }
}

fn worker_busy() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "worker_busy",
        "Active or starting requests are using this worker. Retry after they finish; nothing was saved.",
    )
}

impl Manager {
    pub async fn set_options(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
        options: LaunchOptions,
    ) -> Result<serde_json::Value, ApiError> {
        self.apply_options(id, expected_revision, options, OptionsApply::NextLoad)
            .await
            .map_err(|error| match error {
                OptionsError::Api(error) => error,
                OptionsError::ReloadFailed { status, cause, .. } => ApiError::new(
                    status,
                    cause,
                    "The worker did not start with the new options.",
                ),
            })
    }

    pub async fn apply_options(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
        options: LaunchOptions,
        apply: OptionsApply,
    ) -> Result<serde_json::Value, OptionsError> {
        options
            .validate()
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.code, error.message))?;
        let worker = self.worker(id)?;
        if apply == OptionsApply::Reload {
            self.flush_sessions(&worker).await;
        }
        let mut state = worker.state.clone().lock_owned().await;
        self.ensure_registered(&worker)?;
        if state.revision != expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The profile changed. Refresh before saving options.",
            )
            .into());
        }
        if worker.spec.execution_profile.adapter != Provider::LlamaCpp {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "typed_options_unsupported",
                "Select an explicit llama-cpp provider before using these typed options.",
            )
            .into());
        }
        if worker.spec.execution_profile.native_model_id().is_some() {
            if apply == OptionsApply::Reload {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "reload_unsupported",
                    "Native settings apply on the next load. Unload the worker before changing them.",
                )
                .into());
            }
            drop(state);
            return Ok(self
                .set_native_options(worker, expected_revision, options)
                .await?);
        }
        let restart = match apply {
            OptionsApply::NextLoad => {
                if state.running.is_some()
                    || worker.preparing.load(Ordering::Acquire)
                    || worker.active.load(Ordering::Acquire) != 0
                    || self.processes.snapshot(id).is_some()
                {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "worker_in_use",
                        "Unload this worker before changing its launch profile, or apply the options with a reload. Saved policy is preserved.",
                    )
                    .into());
                }
                false
            }
            OptionsApply::Reload => {
                Self::reap(&mut state);
                if worker.active.load(Ordering::Acquire) != 0
                    || worker.preparing.load(Ordering::Acquire)
                {
                    return Err(worker_busy().into());
                }
                Self::process_available(&worker, state.running.is_some())?;
                state.policy != Policy::Disabled
                    && (state.running.is_some() || state.policy == Policy::Hot)
            }
        };
        let (replacement, previous) = self
            .replace_local_profile(worker, state, move |profile| {
                profile.set_local_options(options).map_err(|_| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "native_settings_authority",
                        "Native options must be changed through their committed settings generation.",
                    )
                })
            })
            .await?;
        let mut view = if restart {
            match self.restart_worker(&replacement).await {
                Ok(()) => self.options(id).await?,
                Err(error) => {
                    return Err(self.roll_back_options(replacement, previous, error).await);
                }
            }
        } else {
            self.options(id).await?
        };
        if apply == OptionsApply::Reload {
            view["apply"] = serde_json::json!("reload");
            view["restarted"] = serde_json::json!(restart);
        }
        Ok(view)
    }

    pub(super) async fn restart_worker(&self, worker: &Arc<Worker>) -> Result<(), ApiError> {
        let state = worker.state.lock().await;
        let revision = state.revision;
        self.ensure_loaded(worker, state, Some(revision))
            .await
            .map(drop)
    }

    async fn roll_back_options(
        self: &Arc<Self>,
        failed: Arc<Worker>,
        previous: ExecutionProfile,
        error: ApiError,
    ) -> OptionsError {
        let state = failed.state.clone().lock_owned().await;
        let restored = if failed.active.load(Ordering::Acquire) != 0
            || failed.preparing.load(Ordering::Acquire)
        {
            None
        } else {
            self.replace_local_profile(failed, state, move |profile| {
                *profile = previous;
                Ok(())
            })
            .await
            .ok()
        };
        let (restored_revision, restarted) = match restored {
            Some((worker, _)) => {
                let revision = worker.state.lock().await.revision;
                (Some(revision), self.restart_worker(&worker).await.is_ok())
            }
            None => (None, false),
        };
        OptionsError::ReloadFailed {
            status: error.status,
            cause: error.code,
            restored_revision,
            restarted,
        }
    }

    async fn replace_local_profile(
        self: &Arc<Self>,
        worker: Arc<Worker>,
        mut state: OwnedMutexGuard<WorkerState>,
        change: impl FnOnce(&mut ExecutionProfile) -> Result<(), ApiError>,
    ) -> Result<(Arc<Worker>, ExecutionProfile), ApiError> {
        let id = worker.spec.id.clone();
        self.ensure_registered(&worker)?;
        let config = self.config.clone().lock_owned().await;
        let mut next = config.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The configuration revision cannot advance.",
            )
        })?;
        let spec = next
            .models
            .iter_mut()
            .find(|model| model.id == id)
            .expect("registered model");
        let previous = spec.execution_profile.clone();
        change(&mut spec.execution_profile)?;
        spec.revision = next.generation;
        let applied = spec
            .execution_profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.code, error.message))?
            .1
            .expect("llama options");
        if applied
            .parallel_slots
            .is_some_and(|slots| slots != spec.max_inflight)
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "slot_resize_requires_registration",
                "Changing physical slot count requires an explicit new registration.",
            ));
        }
        next.validate().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_profile",
                "The saved launch profile is invalid.",
            )
        })?;
        let resolved = next
            .models
            .iter()
            .find(|model| model.id == id)
            .expect("registered model")
            .resolved(self.config_path.parent().expect("configuration parent"));
        let resolved = self.bind_runtime(resolved).await?;
        let replacement = Self::create_worker(
            resolved,
            worker._artifact.clone(),
            &self.admission,
            &self.processes,
            Some(worker.slots.clone()),
        )?;
        let stopped = state.running.is_some();
        if let Some(run) = state.running.as_mut() {
            if let Err(error) = run.child.stop().await {
                state.error = Some("worker_cleanup_pending");
                Self::publish(&worker, &state);
                return Err(super::process_error(error));
            }
            state.running = None;
            state.error = None;
            Self::publish(&worker, &state);
        }
        let previous_worker = worker.clone();
        let outcome = match self
            .publish_configuration(
                config,
                next,
                publication::Effect::Replace {
                    previous: worker,
                    state,
                    replacement: replacement.clone(),
                },
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                if stopped {
                    let _ = self.restart_worker(&previous_worker).await;
                }
                return Err(error);
            }
        };
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Options committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        Ok((replacement, previous))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::tests::manager;

    fn threads(count: u32) -> LaunchOptions {
        LaunchOptions {
            threads: Some(count),
            ..Default::default()
        }
    }

    #[test]
    fn apply_defaults_to_next_load_and_rejects_unknown_modes() {
        assert_eq!(OptionsApply::default(), OptionsApply::NextLoad);
        assert_eq!(
            serde_json::from_str::<OptionsApply>("\"reload\"").unwrap(),
            OptionsApply::Reload
        );
        assert_eq!(
            serde_json::from_str::<OptionsApply>("\"next_load\"").unwrap(),
            OptionsApply::NextLoad
        );
        assert!(serde_json::from_str::<OptionsApply>("\"later\"").is_err());
    }

    #[tokio::test]
    async fn reload_with_active_requests_is_busy_and_saves_nothing() {
        let (_directory, manager) = manager();
        let worker = manager.worker("one").unwrap();
        let before = manager.options("one").await.unwrap();
        worker.active.fetch_add(1, Ordering::AcqRel);
        let error = manager
            .apply_options("one", 1, threads(3), OptionsApply::Reload)
            .await
            .unwrap_err();
        worker.active.fetch_sub(1, Ordering::AcqRel);
        assert_eq!(error.code(), "worker_busy");
        assert_eq!(manager.options("one").await.unwrap(), before);
        assert!(!manager.config_path.exists());
        assert!(Arc::ptr_eq(&manager.worker("one").unwrap(), &worker));
    }

    #[tokio::test]
    async fn failed_hot_reload_restores_previous_options_at_a_newer_revision() {
        let (_directory, manager) = manager();
        let hot = manager.set_policy("one", Policy::Hot, 1).await.unwrap();
        let before = manager.options("one").await.unwrap();
        let error = manager
            .apply_options("one", hot.revision, threads(3), OptionsApply::Reload)
            .await
            .unwrap_err();
        let OptionsError::ReloadFailed {
            cause,
            restored_revision: Some(restored),
            restarted: false,
            ..
        } = error
        else {
            panic!("unexpected reload outcome {error:?}");
        };
        assert_ne!(cause, "options_reload_failed");
        assert!(restored > hot.revision + 1);
        let after = manager.options("one").await.unwrap();
        assert_eq!(after["saved"], before["saved"]);
        assert_eq!(after["revision"], restored);
        let persisted = crate::config::Config::read(&manager.config_path).unwrap();
        let model = &persisted.models[0];
        assert_eq!(model.revision, restored);
        assert_eq!(model.policy, Policy::Hot);
        assert_eq!(
            model
                .execution_profile
                .options()
                .cloned()
                .unwrap_or_default(),
            serde_json::from_value::<LaunchOptions>(before["saved"].clone()).unwrap_or_default()
        );
        let stale = manager
            .apply_options("one", hot.revision, threads(4), OptionsApply::Reload)
            .await
            .unwrap_err();
        assert_eq!(stale.code(), "revision_conflict");
    }
}
