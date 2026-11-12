use super::*;

pub struct RuntimeReply<T> {
    pub(super) value: T,
    pub(super) claim: crate::runtime_packages::memory::Claim,
}

impl<T: serde::Serialize + Send + 'static> RuntimeReply<T> {
    pub async fn into_response(self) -> Result<axum::response::Response, ApiError> {
        crate::bounded_json::respond(
            self.value,
            self.claim,
            crate::runtime_packages::memory::Bounds::selected().response_bytes,
            crate::bounded_json::Domain::Runtime,
        )
        .await
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RuntimeChange {
    Promote { package: String },
    Rollback,
}

pub(super) fn package_error(error: crate::runtime_packages::Error) -> ApiError {
    if error.reason() == "package_target_mismatch" {
        return ApiError::new(
            StatusCode::CONFLICT,
            error.reason(),
            "The package declares a different operating system or CPU target from this service. Select a package for this service target. Matching labels still require separate binary, ABI and hardware qualification.",
        );
    }
    if error.reason() == "runtime_index_publishing" {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            error.reason(),
            "An engine install is still saving its record. Retry in a moment.",
        );
    }
    ApiError::new(
        match error.reason() {
            "package_not_found" => StatusCode::NOT_FOUND,
            "invalid_package_id" => StatusCode::BAD_REQUEST,
            "invalid_import" | "input_unavailable" | "source_bundle_unavailable" => {
                StatusCode::BAD_REQUEST
            }
            "runtime_index_publishing" => StatusCode::SERVICE_UNAVAILABLE,
            "unknown_host_capacity"
            | "host_memory_exhausted"
            | "runtime_memory_limits"
            | "runtime_controller_stopping" => StatusCode::SERVICE_UNAVAILABLE,
            "package_store_unavailable" | "storage_root_unavailable" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            "platform_unsupported" => StatusCode::NOT_IMPLEMENTED,
            "deadline" => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::CONFLICT,
        },
        error.reason(),
        "The installed runtime cannot be used. Preserve its package files and metadata, then inspect the installed runtime inventory before retrying.",
    )
}

struct RuntimeImportOwner(Arc<Manager>);
impl Drop for RuntimeImportOwner {
    fn drop(&mut self) {
        self.0.runtime_importing.store(false, Ordering::Release);
    }
}

impl Manager {
    pub async fn import_runtime(
        self: &Arc<Self>,
        input: PathBuf,
        expected_generation: u64,
    ) -> Result<RuntimeReply<crate::runtime_packages::ImportReceipt>, ApiError> {
        if !cfg!(unix) {
            return Err(package_error(crate::runtime_packages::Error::new(
                "platform_unsupported",
            )));
        }
        if !input.is_absolute()
            || input.as_os_str().len() > 4096
            || input.components().count() > 128
        {
            return Err(package_error(crate::runtime_packages::Error::new(
                "invalid_import",
            )));
        }
        if self.paused.load(Ordering::Acquire) || self.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_import_paused",
                "Runtime installation is unavailable while new work is paused or the service is stopping.",
            ));
        }
        let root = self.config_root.clone().ok_or_else(storage_root_error)?;
        let scope = self.live_roots.clone().ok_or_else(storage_root_error)?;
        self.runtime_importing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                package_error(crate::runtime_packages::Error::new("runtime_import_busy"))
            })?;
        let owner = RuntimeImportOwner(self.clone());
        self.host_memory.refresh(&self.admission).await?;
        let claim = self.runtime_control.import().map_err(ApiError::from)?;
        let mut activity = self.publications.begin()?;
        activity.manager = Some(Arc::downgrade(self));
        let manager = self.clone();
        tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _owner = owner;
                let result = crate::runtime_packages::import_online(&manager.runtime_packages, root, scope, input, expected_generation, Arc::new(AtomicBool::new(false)), manager.runtime_control.clone());
                if !manager.runtime_packages.get().is_some_and(|store| store.publication_uncertain()) { activity.finish(); }
                let value = result.map_err(package_error)?;
                Ok(RuntimeReply { value, claim: claim.response().map_err(ApiError::from)? })
            }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "runtime_import_interrupted", "Runtime installation was interrupted. Preserve its retained files and refresh package inventory before retrying."))?
        }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "runtime_import_interrupted", "Runtime installation did not report completion. Refresh package inventory before retrying."))?
    }

    pub(super) async fn bind_runtime(&self, mut spec: ModelSpec) -> Result<ModelSpec, ApiError> {
        if spec.execution_profile.runtime_package().is_none() {
            return Ok(spec);
        }
        let store = self.runtime_packages.get().cloned();
        self.host_memory.refresh(&self.admission).await?;
        let deadline = Instant::now() + self.startup_timeout;
        tokio::task::spawn_blocking(move || {
            spec.execution_profile
                .bind_runtime_package(store.as_ref(), deadline)
                .map_err(package_error)?;
            Ok(spec)
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_package_interrupted",
                "Installed runtime verification was interrupted. Saved configuration is retained.",
            )
        })?
    }

    pub async fn runtime_packages(&self) -> Result<RuntimeReply<serde_json::Value>, ApiError> {
        self.host_memory.refresh(&self.admission).await?;
        let claim = self.runtime_control.response().map_err(ApiError::from)?;
        let store = self.runtime_packages.get().cloned();
        let active = self.runtime_importing.load(Ordering::Acquire);
        tokio::task::spawn_blocking(move || {
            let mut inventory = if let Some(store) = store {
                // An install publishing its index blocks reads for milliseconds; wait it out.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                loop {
                    match store.inventory() {
                        Err(error)
                            if error.reason() == "runtime_index_publishing"
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        result => break result.map_err(package_error)?,
                    }
                }
            } else {
                serde_json::json!({"schema_version":1,"generation":0,"packages":[],"read_only":true,"compatibility":"unqualified"})
            };
            inventory["installation_active"] = serde_json::json!(active);
            Ok(RuntimeReply { value: inventory, claim })
        })
                .await
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "runtime_inventory_interrupted",
                        "Runtime inventory inspection was interrupted.",
                    )
                })?
    }

    pub async fn runtime_selection(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        let worker = self.worker(id)?;
        let state = worker.state.lock().await;
        self.ensure_registered(&worker)?;
        let profile = &worker.spec.execution_profile;
        let supported = profile.native_model_id().is_none()
            && worker.spec.resource_profile.is_none()
            && profile.adapter != crate::config::Provider::ExternalHttp
            && cfg!(unix);
        let in_use = state.running.is_some()
            || worker.preparing.load(Ordering::Acquire)
            || worker.active.load(Ordering::Acquire) != 0
            || self.processes.snapshot(id).is_some();
        Ok(
            serde_json::json!({"schema_version":1,"model":id,"revision":state.revision,"policy":state.policy,
            "adapter":profile.adapter,"current":profile.runtime_package(),"previous":profile.previous_runtime_package(),
            "supported":supported,"worker_in_use":in_use,"read_only":true,"compatibility":"unqualified"}),
        )
    }

    pub async fn set_runtime_selection(
        self: &Arc<Self>,
        id: String,
        expected_revision: u64,
        change: RuntimeChange,
    ) -> Result<serde_json::Value, ApiError> {
        let mut activity = self.publications.begin()?;
        activity.manager = Some(Arc::downgrade(self));
        let manager = self.clone();
        tokio::spawn(async move {
            let result = manager.change_runtime_selection(&id, expected_revision, change).await;
            activity.finish();
            result
        }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "runtime_revision_interrupted", "Runtime revision publication was interrupted. Refresh the saved model before retrying."))?
    }

    async fn change_runtime_selection(
        self: &Arc<Self>,
        id: &str,
        expected_revision: u64,
        change: RuntimeChange,
    ) -> Result<serde_json::Value, ApiError> {
        let worker = self.worker(id)?;
        let state = worker.state.clone().lock_owned().await;
        self.ensure_registered(&worker)?;
        if state.revision != expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The model changed. Refresh before changing its runtime.",
            ));
        }
        let profile = &worker.spec.execution_profile;
        if profile.native_model_id().is_some()
            || worker.spec.resource_profile.is_some()
            || profile.adapter == crate::config::Provider::ExternalHttp
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "runtime_revision_unsupported",
                "Runtime changes require a local concrete adapter without native-owned settings or a protected resource binding.",
            ));
        }
        if state.running.is_some()
            || worker.preparing.load(Ordering::Acquire)
            || worker.active.load(Ordering::Acquire) != 0
            || self.processes.snapshot(id).is_some()
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_in_use",
                "Unload this worker and finish active requests before changing its runtime. Saved policy is preserved.",
            ));
        }
        let target = match change {
            RuntimeChange::Promote { package } => package,
            RuntimeChange::Rollback => profile
                .previous_runtime_package()
                .map(str::to_owned)
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "runtime_rollback_unavailable",
                        "No previous installed runtime is retained for this model.",
                    )
                })?,
        };
        if profile.runtime_package() == Some(target.as_str()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "runtime_revision_unchanged",
                "This runtime package is already selected.",
            ));
        }
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
        spec.execution_profile.select_runtime_package(target, profile.runtime_package().map(str::to_owned))
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_runtime_revision", "The runtime selection needs an exact installed package identity and local profile authority."))?;
        spec.policy = state.policy;
        spec.revision = next.generation;
        let resolved = spec.resolved(self.config_path.parent().expect("configuration parent"));
        next.validate().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_runtime_revision",
                "The runtime selection cannot form a valid saved profile.",
            )
        })?;
        let next = Self::check_config_size(next).await?;
        let resolved = self.bind_runtime(resolved).await?;
        let replacement = Self::create_worker(
            resolved,
            worker._artifact.clone(),
            &self.admission,
            &self.processes,
            Some(worker.slots.clone()),
        )?;
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
                "Runtime selection committed but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        self.runtime_selection(id).await
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use crate::admission::{Admission, MemoryMode, MemorySettings, Settings};
    use crate::runtime_packages::memory::{Bounds, Controller};
    use futures_util::StreamExt;

    fn controller() -> (Arc<Admission>, Arc<Controller>) {
        let admission = Admission::new(Settings {
            memory: MemorySettings {
                mode: MemoryMode::Protected,
                capacity_bytes: Some(128 * 1024 * 1024),
                essential_headroom_bytes: 0,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        admission.seed_host_sample_for_test(Some(1024 * 1024 * 1024));
        let control = Controller::new(admission.clone());
        (admission, control)
    }

    #[tokio::test]
    async fn runtime_response_slices_keep_host_scratch_and_shutdown_ownership_after_body_drop() {
        let (admission, control) = controller();
        let response = RuntimeReply {
            value: serde_json::json!({"installed":true}),
            claim: control.response().unwrap(),
        }
        .into_response()
        .await
        .unwrap();
        let mut body = response.into_body().into_data_stream();
        let bytes = body.next().await.unwrap().unwrap();
        let retained = bytes.slice(1..);
        drop(body);
        drop(bytes);
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            Bounds::selected().response_work_bytes
        );
        assert!(!control.drain(tokio::time::Instant::now()).await);
        drop(retained);
        assert!(
            control
                .drain(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
        );
        assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
    }

    #[tokio::test]
    async fn oversized_runtime_response_refuses_before_wire_allocation_and_refunds_actual_work() {
        let (admission, control) = controller();
        let claim = control.response().unwrap();
        let error = RuntimeReply {
            value: "x".repeat(Bounds::selected().response_bytes as usize),
            claim,
        }
        .into_response()
        .await
        .unwrap_err();
        assert_eq!(error.code, "runtime_response_limit");
        assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
        assert_eq!(control.projection()["active_operations"], 0);
    }
}
