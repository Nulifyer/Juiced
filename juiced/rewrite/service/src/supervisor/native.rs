use super::*;

fn native_store_error(error: crate::native_store::generations::Error) -> ApiError {
    use crate::native_store::generations::Error as E;
    let status = match error {
        E::Busy => StatusCode::CONFLICT,
        E::Limit | E::Space => StatusCode::INSUFFICIENT_STORAGE,
        E::Unsupported => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    ApiError::new(status, error.code(), error.to_string())
}

fn native_execution_error(error: crate::native_execution::Blocker) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, error.code(), error.to_string())
}

fn native_edit_error(error: crate::native_settings::Error) -> ApiError {
    use crate::native_settings::Reason;
    if matches!(
        error.reason,
        Reason::FileTooLarge | Reason::DepthLimit | Reason::EventLimit
    ) {
        return ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "native_metadata_limit",
            "The proposed native settings exceed their metadata bound. The previous committed generation is unchanged.",
        );
    }
    ApiError::new(StatusCode::CONFLICT, error.code(), error.to_string())
}

fn configuration_digest(
    config: &Config,
) -> Result<crate::native_store::descriptor::Digest, ApiError> {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = HashWriter(Sha256::new());
    serde_json::to_writer(&mut output, config).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_identity",
            "The configuration identity could not be encoded.",
        )
    })?;
    output.0.update(b"\n");
    crate::native_store::descriptor::Digest::new(format!("{:x}", output.0.finalize())).map_err(
        |_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_identity",
                "The configuration identity could not be recorded.",
            )
        },
    )
}

impl Manager {
    pub(super) fn native_model_metadata(
        spec: &ModelSpec,
        artifact: &crate::artifacts::ArtifactLease,
    ) -> Result<crate::profiles::DeclaredContext, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let bundle = spec
            .execution_profile
            .runtime_bundle()
            .ok_or("Native runtime bindings require an explicit verified runtime bundle")?
            .verify(deadline)
            .map_err(|_| "The native runtime bundle could not be verified")?;
        if crate::identity::executable_path(&spec.execution_profile.command)? != bundle.executable {
            return Err("The native command does not match its verified bundle entrypoint".into());
        }
        let file = crate::identity::VerifiedFile::open(artifact.path(), deadline)?;
        if Some(file.digest()) != spec.artifact.as_deref() {
            return Err("The native model artifact binding changed".into());
        }
        let metadata = crate::profiles::read_gguf(&mut file.metadata_reader()?)?;
        file.verify_current()?;
        Ok(metadata)
    }

    pub(super) fn bind_native_model(
        spec: &mut ModelSpec,
        artifact: &crate::artifacts::ArtifactLease,
        current: Arc<NativeSettingsSnapshot>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        spec.model_file = artifact.path().to_owned();
        let metadata = Self::native_model_metadata(spec, artifact)?;
        current.generation.verify()?;
        let view = current.context.resolve(
            spec.execution_profile
                .native_model_id()
                .ok_or("A native model binding is required")?,
            &metadata,
            spec.max_inflight,
        )?;
        spec.execution_profile.bind_native_execution(
            view.options().clone(),
            view.extra_arguments().to_vec(),
            view.semantic_digest().to_owned(),
            current.generation.clone(),
        )?;
        let applied = spec
            .execution_profile
            .resolved_arguments()?
            .1
            .ok_or("Native launch options are missing")?;
        if applied.parallel_slots != Some(spec.max_inflight) {
            return Err("The native physical slot binding changed".into());
        }
        Ok(())
    }

    pub async fn import_native_copy(
        self: &Arc<Self>,
        source: crate::native_store::SourceCohort,
        bindings: Vec<ModelSpec>,
    ) -> Result<serde_json::Value, ApiError> {
        if !self.native_import_only {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "offline_native_import_required",
                "Open an exclusive offline import owner before importing copied native metadata.",
            ));
        }
        let base = self.config.lock().await.clone();
        if base.native_settings.is_some() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "native_settings_already_owned",
                "This configuration already owns a native settings generation.",
            ));
        }
        let expected_generation = base.generation;
        let expected_digest = configuration_digest(&base)?;
        let manager = self.clone();
        let preparation_digest = expected_digest.clone();
        let (next, snapshot, workers, count) = tokio::task::spawn_blocking(move || {
            let native = manager.native_settings.as_ref().ok_or_else(|| ApiError::new(
                StatusCode::CONFLICT, "native_settings_owner_required", "The private native metadata owner is unavailable."))?;
            source.verify().map_err(|_| ApiError::new(StatusCode::CONFLICT, "native_source_changed", "The copied native metadata is unavailable, changed or outside its inspection deadline."))?;
            let context = Arc::new(crate::native_execution::ResolutionContext::from_source(&source).map_err(native_execution_error)?);
            let mut next = base;
            next.schema_version = 3;
            next.generation = next.generation.checked_add(1).ok_or_else(|| ApiError::new(
                StatusCode::CONFLICT, "revision_exhausted", "The configuration generation cannot advance."))?;
            let first_imported = next.models.len();
            for mut model in bindings {
                if model.policy != Policy::Disabled || model.revision != 0 || model.execution_profile.native_model_id().is_none() {
                    return Err(ApiError::new(StatusCode::BAD_REQUEST, "native_import_binding", "Native import bindings need a native-owned profile, Disabled policy and revision zero."));
                }
                model.revision = next.generation;
                next.models.push(model);
            }
            let plan = native.store.plan_import(&source, preparation_digest).map_err(native_store_error)?;
            next.native_settings = Some(plan.binding().clone());
            next.validate().map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "native_import_binding", "The native model bindings conflict with this configuration."))?;
            next.check_persisted_size().map_err(configuration_size_error)?;
            let artifacts = manager.artifacts()?;
            let mut validated = Vec::with_capacity(next.models.len() - first_imported);
            for model in &mut next.models[first_imported..] {
                let artifact = model.artifact.as_deref().ok_or_else(|| ApiError::new(
                    StatusCode::BAD_REQUEST, "native_artifact_binding", "Native runtime bindings require a managed model artifact."))?;
                let held = Arc::new(artifacts.hold_binding(artifact, model.artifact_set.as_deref()).map_err(ApiError::from)?);
                model.model_file = held.path().to_owned();
                let resolved = model.resolved(manager.config_path.parent().expect("configuration parent"));
                let metadata = Self::native_model_metadata(&resolved, &held).map_err(|_| ApiError::new(
                    StatusCode::CONFLICT, "native_artifact_binding", "The native model and runtime bundle binding could not be verified."))?;
                let view = context.resolve(model.execution_profile.native_model_id().expect("validated native profile"), &metadata, model.max_inflight).map_err(native_execution_error)?;
                model.execution_profile.bind_native_options(view.options().clone()).map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?;
                validated.push((held, view));
                source.verify().map_err(|_| ApiError::new(StatusCode::CONFLICT, "native_source_changed", "The copied native metadata changed during binding inspection."))?;
            }
            next.validate().map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "native_import_binding", "The derived native launch profile is invalid."))?;
            next.check_persisted_size().map_err(configuration_size_error)?;
            let prepared = native.store.prepare_import(plan).map_err(native_store_error)?;
            let snapshot = Arc::new(NativeSettingsSnapshot { generation: Arc::new(prepared), context });
            let mut workers = Vec::with_capacity(validated.len());
            for (model, (artifact, view)) in next.models[first_imported..].iter_mut().zip(validated) {
                model.execution_profile.bind_native_execution(view.options().clone(), view.extra_arguments().to_vec(), view.semantic_digest().to_owned(), snapshot.generation.clone())
                    .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?;
                artifacts.reference_binding(&artifact.artifact().id, artifact.set_id(), &format!("model:{}", model.id), true, artifacts.generation()).map_err(ApiError::from)?;
                let resolved = model.resolved(manager.config_path.parent().expect("configuration parent"));
                workers.push(Self::create_worker(resolved, Some(artifact), &manager.admission, &manager.processes, None)?);
            }
            snapshot.generation.verify().map_err(native_store_error)?;
            let count = next.models.len() - first_imported;
            Ok::<_, ApiError>((next, snapshot, workers, count))
        }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "native_preparation_interrupted", "Native metadata preparation was interrupted. Preserve its private candidate files; the configuration was not published by this step."))??;
        let config = self.config.clone().lock_owned().await;
        if config.generation != expected_generation
            || configuration_digest(&config)? != expected_digest
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Configuration changed during native preparation. The prepared candidate was retained without activation.",
            ));
        }
        let outcome = self
            .publish_configuration(
                config,
                next,
                publication::Effect::NativeImport { snapshot, workers },
            )
            .await?;
        if outcome.warning {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "The native settings generation was committed, but directory durability is unconfirmed. Inspect the current configuration before retrying.",
            ));
        }
        Ok(
            serde_json::json!({"schema_version":1,"configuration_generation":outcome.generation,"models_added":count,"imported_policy":"disabled","source_modified":false,"scope":"copied legacy metadata; no native helper or job replay"}),
        )
    }

    pub(super) async fn native_options(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        if self.publications.is_uncertain() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_publication_uncertain",
                "Configuration publication requires restart before native settings can be observed as current.",
            ));
        }
        let config = self.config.lock().await;
        let model = config
            .models
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_model",
                    "The model is not registered.",
                )
            })?;
        let native_id = model
            .execution_profile
            .native_model_id()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "model_replaced",
                    "The model settings owner changed. Refresh its current registration.",
                )
            })?
            .to_owned();
        let revision = model.revision;
        let physical_slots = model.max_inflight;
        let worker = self.worker(id)?;
        let profile = worker.spec.execution_profile.clone();
        let native = self.native_settings.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "native_settings_owner_required",
                "The private native settings owner is unavailable.",
            )
        })?;
        let snapshot = native
            .current
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "native_settings_recovery_required",
                    "The native settings generation is missing.",
                )
            })?;
        drop(config);
        let generation = snapshot.generation.clone();
        tokio::task::spawn_blocking(move || generation.verify())
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "native_settings_recovery_required",
                    "Native settings observation was interrupted.",
                )
            })?
            .map_err(native_store_error)?;
        let saved = snapshot
            .context
            .settings(&native_id)
            .map_err(native_execution_error)?
            .saved_options(&native_id)
            .map_err(|_| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "model_settings_recovery_required",
                    "Stored native model options require recovery.",
                )
            })?;
        let context_tokens = saved
            .get("ctx_size")
            .map(|value| serde_json::from_str::<Option<u32>>(value.get()))
            .transpose()
            .map_err(|_| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "model_settings_recovery_required",
                    "The saved native context is outside the selected typed options view.",
                )
            })?
            .flatten();
        let projection = crate::profiles::LaunchOptions {
            context_tokens,
            ..Default::default()
        };
        let effective = profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?
            .1;
        Ok(
            serde_json::json!({"model":id,"revision":revision,"provider":profile.adapter,"options_authority":"native-settings","editable_fields":["context_tokens"],"saved":projection,"effective_launch":effective,"physical_slots":physical_slots,"context_semantics":"context_tokens is a total launch budget; serving context requires engine observation"}),
        )
    }

    fn native_edit_guard(
        &self,
        worker: &Arc<Worker>,
        state: &WorkerState,
        expected_revision: u64,
    ) -> Result<(), ApiError> {
        self.ensure_registered(worker)?;
        if state.revision != expected_revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The native settings changed. Refresh before saving options.",
            ));
        }
        if state.running.is_some()
            || worker.preparing.load(Ordering::Acquire)
            || worker.active.load(Ordering::Acquire) != 0
            || self.processes.snapshot(&worker.spec.id).is_some()
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_in_use",
                "Unload this worker before changing its native settings. Saved policy is preserved.",
            ));
        }
        if self.publications.is_uncertain() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_publication_uncertain",
                "Configuration publication requires restart before native settings can be changed.",
            ));
        }
        Ok(())
    }

    pub(super) async fn set_native_options(
        self: &Arc<Self>,
        worker: Arc<Worker>,
        expected_revision: u64,
        options: crate::profiles::LaunchOptions,
    ) -> Result<serde_json::Value, ApiError> {
        if options.threads.is_some()
            || options.gpu_layers.is_some()
            || options.parallel_slots.is_some()
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "native_options_authority",
                "This native settings view edits only the context override; CPU placement and the physical slot remain bound by the imported profile.",
            ));
        }
        let state = worker.state.lock().await;
        self.native_edit_guard(&worker, &state, expected_revision)?;
        let config = self.config.lock().await;
        let base = config.clone();
        let expected_generation = base.generation;
        let expected_digest = configuration_digest(&base)?;
        let native = self.native_settings.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "native_settings_owner_required",
                "The private native settings owner is unavailable.",
            )
        })?;
        let current = native
            .current
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "native_settings_recovery_required",
                    "The current native settings generation is missing.",
                )
            })?;
        if base.native_settings.as_ref() != Some(current.generation.binding()) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "native_settings_recovery_required",
                "The native settings descriptor and its current view disagree.",
            ));
        }
        drop(config);
        drop(state);
        let manager = self.clone();
        let previous = worker.clone();
        let preparation_digest = expected_digest.clone();
        let (next, snapshot, replacement) = tokio::task::spawn_blocking(move || {
            let native_id = previous.spec.execution_profile.native_model_id().ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "model_replaced", "The native profile binding is missing."))?;
            current.generation.verify().map_err(native_store_error)?;
            let settings = current.context.settings(native_id).map_err(native_execution_error)?;
            let patch = crate::native_settings::SavedOptions::parse(&serde_json::to_vec(&serde_json::json!({"ctx_size":options.context_tokens})).map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_native_options", "The native context patch could not be encoded."))?).map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_native_options", "The native context patch is invalid."))?;
            let changed_settings = settings.merge_options(native_id, &patch).map_err(native_edit_error)?;
            let context = current.context.with_settings(&changed_settings).map_err(native_execution_error)?;
            let changed = [crate::native_settings::Role::RecipeOptions, crate::native_settings::Role::ExecutionProfiles].into_iter().filter_map(|role| {
                let old = settings.document(role).expect("settings role");
                let new = changed_settings.document(role).expect("settings role");
                (!old.same_view(new)).then(|| new.clone())
            }).collect::<Vec<_>>();
            let native = manager.native_settings.as_ref().expect("native settings owner");
            let plan = native.store.plan_edit(&current.generation, &changed, preparation_digest).map_err(native_store_error)?;
            let mut next = base;
            next.generation = next.generation.checked_add(1).ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "revision_exhausted", "The configuration generation cannot advance."))?;
            next.native_settings = Some(plan.binding().clone());
            let model = next.models.iter_mut().find(|model| model.id == previous.spec.id).expect("registered native model");
            model.revision = next.generation;
            let artifact = previous._artifact.as_ref().ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "native_artifact_binding", "The native model artifact is unavailable."))?;
            model.model_file = artifact.path().to_owned();
            let resolved = model.resolved(manager.config_path.parent().expect("configuration parent"));
            let metadata = Self::native_model_metadata(&resolved, artifact).map_err(|_| ApiError::new(StatusCode::CONFLICT, "native_artifact_binding", "The native model or runtime bundle changed."))?;
            let view = context.resolve(native_id, &metadata, model.max_inflight).map_err(native_execution_error)?;
            model.execution_profile.bind_native_options(view.options().clone()).map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?;
            next.validate().map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_native_profile", "The native launch settings are invalid."))?;
            next.check_persisted_size().map_err(configuration_size_error)?;
            let prepared = native.store.prepare_edit(plan).map_err(native_store_error)?;
            let context = context.rebased(&prepared).map_err(native_execution_error)?;
            let snapshot = Arc::new(NativeSettingsSnapshot { generation: Arc::new(prepared), context: Arc::new(context) });
            let model = next.models.iter_mut().find(|model| model.id == previous.spec.id).expect("registered native model");
            model.execution_profile.bind_native_execution(view.options().clone(), view.extra_arguments().to_vec(), view.semantic_digest().to_owned(), snapshot.generation.clone()).map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?;
            let resolved = model.resolved(manager.config_path.parent().expect("configuration parent"));
            let replacement = Self::create_worker(resolved, previous._artifact.clone(), &manager.admission, &manager.processes, Some(previous.slots.clone()))?;
            Ok::<_, ApiError>((next, snapshot, replacement))
        }).await.map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "native_preparation_interrupted", "Native settings preparation was interrupted. The candidate remains unactivated until a configuration publication succeeds."))??;
        let state = worker.state.clone().lock_owned().await;
        self.native_edit_guard(&worker, &state, expected_revision)?;
        let config = self.config.clone().lock_owned().await;
        if config.generation != expected_generation
            || configuration_digest(&config)? != expected_digest
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Configuration changed during native preparation. The previous committed generation remains authoritative.",
            ));
        }
        let outcome = self
            .publish_configuration(
                config,
                next,
                publication::Effect::NativeEdit {
                    snapshot,
                    previous: worker.clone(),
                    state,
                    replacement,
                },
            )
            .await?;
        if outcome.warning {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Native options were committed, but directory durability is unconfirmed. Refresh before retrying.",
            ));
        }
        self.native_options(&worker.spec.id).await
    }
}
