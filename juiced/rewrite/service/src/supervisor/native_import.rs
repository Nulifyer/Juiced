use super::*;
use crate::{
    engines::{ChoiceError, catalog::Engine, models},
    native_import::{Candidate, Inspection, MAX_CANDIDATES, Origin, Refusal, RefusalKind, Weights},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NativeImportRequest {
    Preview {
        source: PathBuf,
        #[serde(default)]
        weights: Option<PathBuf>,
    },
    Apply {
        source: PathBuf,
        #[serde(default)]
        weights: Option<PathBuf>,
        models: Vec<NativeImportModel>,
        #[serde(default)]
        aliases: bool,
        expected_generation: u64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeImportModel {
    pub native_id: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub policy: Option<Policy>,
    #[serde(default = "included")]
    pub include: bool,
}

fn included() -> bool {
    true
}

fn refusal(error: Refusal) -> ApiError {
    let status = match error.kind {
        RefusalKind::BadRequest => StatusCode::BAD_REQUEST,
        RefusalKind::Forbidden => StatusCode::FORBIDDEN,
        RefusalKind::NotFound => StatusCode::NOT_FOUND,
        RefusalKind::Conflict => StatusCode::CONFLICT,
        RefusalKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    ApiError::new(status, error.code, error.message)
}

fn interrupted() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "native_import_interrupted",
        "Native import inspection was interrupted. Nothing was registered and the source was not modified.",
    )
}

fn blocker(code: &'static str) -> Value {
    let message = match code {
        "native_definition_missing" => {
            "The model has saved settings but no registration in the native registry or user_models.json."
        }
        "native_checkpoint_missing" => "The native registration names no checkpoint.",
        "native_checkpoint_invalid" => {
            "The native checkpoint is not a Hugging Face repository name."
        }
        "weights_root_missing" => {
            "No Hugging Face cache was found under the import roots. Mount the old huggingface directory or pass weights."
        }
        "weights_missing" => "The weights are not in the old Hugging Face cache.",
        "weights_outside_import_roots" => {
            "The registered local file is not under a configured import root."
        }
        "weights_registry_unsupported" => {
            "Only Hugging Face cache layouts are imported; this model came from another registry."
        }
        "native_recipe_unsupported" => "The rewrite has no engine for this native recipe.",
        "native_recipe_mismatch" => "The weight file format does not match the native recipe.",
        "companions_required" => {
            "This model needs companion files (voices, VAE or text encoders). Publish an artifact set and add it instead."
        }
        "no_usable_backend" => {
            "Hardware detection found no available core backend for this engine."
        }
        "extra_models_unsupported" => "Models from the native extra_models_dir are not imported.",
        "native_row_unrecognized" => {
            "Saved settings refer to a model that is not in the native registry or user_models.json. They stay in the source."
        }
        "model_settings_recovery_required" => {
            "This model's saved native options are damaged. They were not imported or changed."
        }
        "artifact_unavailable" => "The weight file could not be inspected.",
        "unsupported_model_format" => {
            "The weight file is not a GGUF, whisper.cpp ggml, diffusion safetensors or ONNX model."
        }
        _ => "The model metadata is invalid or exceeds inspection limits.",
    };
    json!({"code": code, "message": message})
}

struct Planned {
    candidate: Candidate,
    weights: Result<Weights, &'static str>,
    blockers: Vec<Value>,
    row: serde_json::Map<String, Value>,
    registration: Option<Value>,
    install: Option<Value>,
}

impl Planned {
    fn status(&self) -> &'static str {
        if !self.blockers.is_empty() {
            "blocked"
        } else if self.install.is_some() {
            "engine_install_needed"
        } else {
            "ready"
        }
    }
}

struct Plan {
    inspection: Arc<Inspection>,
    models: Vec<Planned>,
    packages_generation: Value,
}

impl Manager {
    async fn native_import_plan(
        &self,
        source: PathBuf,
        weights: Option<PathBuf>,
    ) -> Result<Plan, ApiError> {
        let roots = self.config.lock().await.import_roots.clone();
        let (inspection, detected) = tokio::task::spawn_blocking(move || {
            let inspection = Inspection::open(
                &source,
                weights.as_deref(),
                &roots,
                Arc::new(AtomicBool::new(false)),
            )
            .map_err(refusal)?;
            let detected: Vec<_> = inspection
                .candidates
                .iter()
                .map(|candidate| {
                    let weights = inspection.find_weights(candidate);
                    let format = weights
                        .as_ref()
                        .ok()
                        .map(|weights| models::detect(&weights.path).map(|(format, _)| format));
                    let mmproj = candidate
                        .definition
                        .as_ref()
                        .and_then(|definition| definition.mmproj.as_deref())
                        .map(|checkpoint| inspection.find_companion(checkpoint));
                    (weights, format, mmproj)
                })
                .collect();
            Ok::<_, ApiError>((Arc::new(inspection), detected))
        })
        .await
        .map_err(|_| interrupted())??;
        let (snapshot, inventory) = self.host().await?;
        let packages = self.package_inventory().await?;
        let installed = crate::engines::installed_packages(&packages);
        let cpu_threads = snapshot
            .report
            .devices
            .cpu
            .as_ref()
            .and_then(|cpu| cpu.physical_cores.or(cpu.usable_threads));
        let mut planned = Vec::with_capacity(detected.len());
        for (candidate, (weights, format, mmproj)) in
            inspection.candidates.iter().cloned().zip(detected)
        {
            let mut blockers = Vec::new();
            let mut row = serde_json::Map::new();
            let definition = candidate.definition.clone();
            let recipe = definition
                .as_ref()
                .and_then(|definition| definition.recipe.clone());
            match candidate.origin {
                Origin::Extra => blockers.push(blocker("extra_models_unsupported")),
                Origin::Unrecognized => blockers.push(blocker("native_row_unrecognized")),
                _ => {}
            }
            if matches!(candidate.origin, Origin::Builtin | Origin::User)
                && candidate.saved.is_none()
            {
                blockers.push(blocker("model_settings_recovery_required"));
            }
            let format = match format {
                Some(Ok(format)) => Some(format),
                Some(Err(error)) => {
                    blockers.push(blocker(match error {
                        models::FormatError::Unavailable => "artifact_unavailable",
                        models::FormatError::Unsupported => "unsupported_model_format",
                        models::FormatError::Invalid(code) => code,
                    }));
                    None
                }
                None => None,
            };
            if let Err(code) = &weights
                && matches!(candidate.origin, Origin::Builtin | Origin::User)
            {
                blockers.push(blocker(code));
            }
            let recipe_engine = recipe.as_deref().and_then(Engine::parse);
            if matches!(candidate.origin, Origin::Builtin | Origin::User) && recipe_engine.is_none()
            {
                blockers.push(blocker("native_recipe_unsupported"));
            }
            let engine = format.as_ref().map(models::ModelFormat::engine);
            if let (Some(engine), Some(recipe_engine)) = (engine, recipe_engine)
                && engine != recipe_engine
            {
                blockers.push(blocker("native_recipe_mismatch"));
            }
            if let Some(format) = &format
                && models::companion_arguments(format, &[]).is_err()
            {
                blockers.push(blocker("companions_required"));
            }
            let empty = crate::native_settings::SavedOptions::default();
            let mapped = crate::native_import::map_options(
                recipe_engine,
                recipe.as_deref().unwrap_or_default(),
                &inspection.config,
                definition
                    .as_ref()
                    .map(|definition| &definition.registered)
                    .unwrap_or(&BTreeMap::new()),
                candidate.saved.as_ref().unwrap_or(&empty),
            );
            let mut settings = mapped.rows.clone();
            if let Some(definition) = &definition {
                if let Some(checkpoint) = &definition.mmproj {
                    settings.push(crate::native_import::SettingRow {
                        scope: "registration",
                        key: "mmproj".into(),
                        state: "unsupported",
                        value: Some(json!({"checkpoint": checkpoint, "found": mmproj.unwrap_or(false)})),
                        target: None,
                        reason: "Vision projectors are not imported yet; the model is imported for text only.",
                    });
                }
                for role in &definition.other_checkpoints {
                    settings.push(crate::native_import::SettingRow {
                        scope: "registration",
                        key: format!("checkpoints.{role}"),
                        state: "unsupported",
                        value: None,
                        target: None,
                        reason: "Draft and auxiliary checkpoints are not imported.",
                    });
                }
            }
            let mut install = None;
            let mut registration = None;
            let mut engine_view = Value::Null;
            if let (Some(engine), Some(format), Ok(found), true) =
                (engine, &format, &weights, blockers.is_empty())
            {
                let requested = mapped.backend.as_deref();
                let mut native_backend_note = None;
                let mut choice = crate::engines::choose(
                    engine,
                    &inventory,
                    &snapshot.report,
                    &installed,
                    requested,
                );
                if matches!(choice, Err(ChoiceError::NotSelectable(_))) {
                    native_backend_note = Some(
                        "The native backend is not available on this host; the hardware recommendation is used.",
                    );
                    choice = crate::engines::choose(
                        engine,
                        &inventory,
                        &snapshot.report,
                        &installed,
                        None,
                    );
                }
                match choice {
                    Ok(choice) => {
                        let context = match format {
                            models::ModelFormat::Gguf { .. } => mapped
                                .options
                                .context_tokens
                                .map(u64::from)
                                .or_else(|| models::context_tokens(format)),
                            _ => None,
                        };
                        let estimate = models::estimate(format, found.bytes, context);
                        let mut value = models::registration(&models::ProfileInputs {
                            id: &candidate.public_name,
                            model_file: &found.path,
                            artifact: None,
                            artifact_set: None,
                            package: &choice.package,
                            backend: &choice.backend,
                            gpu: choice.gpu,
                            cpu_threads,
                            format,
                            estimate: &estimate,
                            companion_arguments: &[],
                        });
                        if matches!(format, models::ModelFormat::Gguf { .. }) {
                            let options = &mut value["execution_profile"]["options"];
                            if choice.gpu
                                && let Some(layers) = mapped.options.gpu_layers
                            {
                                options["gpu_layers"] = json!(layers);
                            }
                            if !choice.gpu
                                && let Some(threads) = mapped.options.threads
                            {
                                options["threads"] = json!(threads);
                            }
                        }
                        engine_view = json!({
                            "engine": engine.name(), "backend": choice.backend, "package": choice.package,
                            "installed": true, "engine_install_needed": false, "install": null,
                            "reason": native_backend_note.map_or(choice.reason.clone(), |note| format!("{note} {}", choice.reason)),
                            "native_backend": mapped.backend,
                        });
                        row.insert("estimate".into(), json!(estimate));
                        row.insert(
                            "execution_profile".into(),
                            value["execution_profile"].clone(),
                        );
                        registration = Some(value);
                    }
                    Err(ChoiceError::NotInstalled {
                        backend,
                        gpu_target,
                        installable,
                    }) => {
                        let payload = json!({
                            "engine": engine.name(), "backend": backend, "gpu_target": gpu_target,
                            "installable": installable, "expected_generation": packages["generation"],
                        });
                        engine_view = json!({
                            "engine": engine.name(), "backend": backend, "package": null,
                            "installed": false, "engine_install_needed": true, "install": payload,
                            "reason": native_backend_note.map_or_else(
                                || format!("{backend} is the recommended {} backend on this host and is not installed.", engine.name()),
                                |note| format!("{note} Install {backend} first."),
                            ),
                            "native_backend": mapped.backend,
                        });
                        install = Some(payload);
                    }
                    Err(_) => blockers.push(blocker("no_usable_backend")),
                }
            }
            let suggested = match candidate.previous_policy.map(|(policy, _)| policy) {
                Some("hot") => "hot",
                Some("on-demand") => "on-demand",
                _ => "disabled",
            };
            let definition_view = definition.as_ref();
            row.insert("native_id".into(), json!(candidate.native_id));
            row.insert("proposed_id".into(), json!(candidate.public_name));
            row.insert("origin".into(), json!(candidate.origin));
            row.insert(
                "recipe".into(),
                json!(definition_view.and_then(|definition| definition.recipe.clone())),
            );
            row.insert(
                "checkpoint".into(),
                json!(definition_view.and_then(|definition| definition.checkpoint.clone())),
            );
            row.insert(
                "labels".into(),
                json!(
                    definition_view
                        .map(|definition| definition.labels.clone())
                        .unwrap_or_default()
                ),
            );
            row.insert("selected_profile".into(), json!(candidate.selected_profile));
            row.insert(
                "weights".into(),
                match &weights {
                    Ok(found) => json!({"found": true, "path": found.path, "bytes": found.bytes, "files": found.files.len(), "selection": found.selection, "snapshot": found.snapshot}),
                    Err(code) => json!({"found": false, "reason": code}),
                },
            );
            row.insert("format".into(), json!(format));
            row.insert("engine".into(), engine_view);
            row.insert("options".into(), json!(mapped.options));
            row.insert("settings".into(), json!(settings));
            row.insert(
                "previous_policy".into(),
                json!(candidate.previous_policy.map(|(policy, _)| policy)),
            );
            row.insert(
                "previous_policy_source".into(),
                json!(candidate.previous_policy.map(|(_, source)| source)),
            );
            row.insert("suggested_policy".into(), json!(suggested));
            row.insert("default_policy".into(), json!("disabled"));
            planned.push(Planned {
                candidate,
                weights,
                blockers,
                row,
                registration,
                install,
            });
        }
        Ok(Plan {
            inspection,
            models: planned,
            packages_generation: packages["generation"].clone(),
        })
    }

    fn import_conflicts(
        config: &Config,
        id: &str,
        weights: Option<&Weights>,
    ) -> (Option<String>, Vec<Value>) {
        let mut conflicts = Vec::new();
        let mut already = None;
        if let Some(model) = config.models.iter().find(|model| model.id == id) {
            if weights.is_some_and(|weights| {
                std::fs::canonicalize(&model.model_file).is_ok_and(|path| path == weights.path)
            }) {
                already = Some(model.id.clone());
            } else {
                conflicts.push(json!({"kind": "model_id", "id": id, "reason": "A different model is registered with this ID. Choose another ID."}));
            }
        }
        if config.aliases.entries.contains_key(id) {
            conflicts.push(json!({"kind": "alias", "id": id, "reason": "This ID is saved as an alias. Delete or rename the alias, or choose another ID."}));
        }
        (already, conflicts)
    }

    pub async fn native_import(
        self: &Arc<Self>,
        request: NativeImportRequest,
    ) -> Result<Value, ApiError> {
        match request {
            NativeImportRequest::Preview { source, weights } => {
                self.native_import_preview(source, weights).await
            }
            NativeImportRequest::Apply {
                source,
                weights,
                models,
                aliases,
                expected_generation,
            } => {
                self.native_import_apply(source, weights, models, aliases, expected_generation)
                    .await
            }
        }
    }

    async fn native_import_preview(
        self: &Arc<Self>,
        source: PathBuf,
        weights: Option<PathBuf>,
    ) -> Result<Value, ApiError> {
        let plan = self.native_import_plan(source, weights).await?;
        let config = self.config.lock().await.clone();
        let mut rows = Vec::with_capacity(plan.models.len());
        let mut proposed: BTreeMap<String, String> = BTreeMap::new();
        for model in plan.models {
            let (already, conflicts) = Self::import_conflicts(
                &config,
                &model.candidate.public_name,
                model.weights.as_ref().ok(),
            );
            let status = match (&already, model.status()) {
                (Some(_), "ready" | "engine_install_needed") => "already_imported",
                (_, status) => status,
            };
            if status != "blocked" {
                proposed.insert(
                    model.candidate.public_name.clone(),
                    model.candidate.native_id.clone(),
                );
            }
            let mut row = model.row;
            row.insert("status".into(), json!(status));
            row.insert(
                "importable".into(),
                json!(status == "ready" || status == "engine_install_needed"),
            );
            row.insert("already_imported".into(), json!(already));
            row.insert("conflicts".into(), json!(conflicts));
            row.insert("blockers".into(), json!(model.blockers));
            rows.push(Value::Object(row));
        }
        let aliases: Vec<Value> = plan
            .inspection
            .aliases
            .iter()
            .map(|(alias, target)| {
                let (state, reason) = if config.models.iter().any(|model| model.id == *alias)
                    || proposed.contains_key(alias)
                {
                    ("conflict", "The alias name is a model ID.")
                } else if config.aliases.entries.get(alias) == Some(target) {
                    ("unchanged", "The alias is already saved with this target.")
                } else if config.aliases.entries.contains_key(alias) {
                    ("conflict", "The alias is already saved with another target and will not be overwritten.")
                } else if proposed.contains_key(target) || config.models.iter().any(|model| model.id == *target) {
                    ("importable", "Added when its target model is imported with its proposed ID.")
                } else {
                    ("target_not_imported", "The alias target is not importable from this source.")
                };
                json!({"alias": alias, "target": target, "target_native_id": proposed.get(target), "state": state, "reason": reason})
            })
            .collect();
        Ok(json!({
            "schema_version": 1,
            "action": "preview",
            "source": plan.inspection.source,
            "weights_root": plan.inspection.weights_root,
            "configuration_generation": config.generation,
            "engines_generation": plan.packages_generation,
            "documents": plan.inspection.documents,
            "models": rows,
            "truncated": plan.inspection.truncated,
            "aliases": aliases,
            "global_settings": crate::native_import::global_rows(&plan.inspection.config),
            "default_policy": "disabled",
            "source_modified": false,
            "scope": "Weights are referenced in place and never copied. Native jobs, helpers, snapshots and prompt state are not imported.",
        }))
    }

    async fn native_import_apply(
        self: &Arc<Self>,
        source: PathBuf,
        weights: Option<PathBuf>,
        requested: Vec<NativeImportModel>,
        include_aliases: bool,
        expected_generation: u64,
    ) -> Result<Value, ApiError> {
        if requested.len() > MAX_CANDIDATES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "native_import_limit",
                format!("Import at most {MAX_CANDIDATES} models per request."),
            ));
        }
        let mut native_ids = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for model in &requested {
            if !native_ids.insert(model.native_id.as_str()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "duplicate_native_model",
                    "Each native model may appear once in an import request.",
                ));
            }
            if let Some(id) = &model.id
                && !crate::config::valid_model_id(id)
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_model_id",
                    "Model IDs must be nonempty literal text of at most 256 bytes without commas or surrounding whitespace.",
                ));
            }
        }
        if self.config.lock().await.generation != expected_generation {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The configuration changed. Preview the import again before applying it.",
            ));
        }
        let plan = self.native_import_plan(source, weights).await?;
        let mut by_native: BTreeMap<String, Planned> = plan
            .models
            .into_iter()
            .map(|model| (model.candidate.native_id.clone(), model))
            .collect();
        let snapshot = self.config.lock().await.clone();
        let mut results = Vec::with_capacity(requested.len());
        let mut specs = Vec::new();
        let mut policies = Vec::new();
        let mut resulting: BTreeMap<String, String> = BTreeMap::new();
        for request in requested {
            let model = by_native.remove(&request.native_id).ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "native_model_unknown",
                    "A requested native model is not in this source. Preview the import again.",
                )
            })?;
            let id = request
                .id
                .clone()
                .unwrap_or_else(|| model.candidate.public_name.clone());
            if !request.include {
                results
                    .push(json!({"native_id": request.native_id, "id": id, "status": "skipped"}));
                continue;
            }
            if !ids.insert(id.clone()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "duplicate_model_id",
                    "Two imported models would share one model ID.",
                ));
            }
            let status = model.status();
            if status == "blocked" {
                results.push(json!({"native_id": request.native_id, "id": id, "status": "blocked", "blockers": model.blockers}));
                continue;
            }
            let (already, conflicts) =
                Self::import_conflicts(&snapshot, &id, model.weights.as_ref().ok());
            if let Some(existing) = already {
                resulting.insert(model.candidate.public_name.clone(), existing.clone());
                let policy = snapshot
                    .models
                    .iter()
                    .find(|spec| spec.id == existing)
                    .map(|spec| spec.policy);
                results.push(json!({"native_id": request.native_id, "id": existing, "status": "already_imported", "policy": policy}));
                continue;
            }
            if let Some(conflict) = conflicts.first() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    if conflict["kind"] == "alias" {
                        "alias_conflict"
                    } else {
                        "model_exists"
                    },
                    "An imported model ID is already registered or saved as an alias. Choose another ID.",
                ));
            }
            if let Some(install) = model.install {
                results.push(json!({"native_id": request.native_id, "id": id, "status": "engine_install_needed", "engine_install_needed": true, "install": install}));
                continue;
            }
            let mut value = model.registration.expect("ready import registration");
            value["id"] = json!(id);
            let crate::config::Registration(spec) =
                serde_json::from_value(value).map_err(|_| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "automatic_profile_invalid",
                        "The imported execution profile was rejected by the registration schema.",
                    )
                })?;
            resulting.insert(model.candidate.public_name.clone(), id.clone());
            let policy = request.policy.unwrap_or(Policy::Disabled);
            if policy != Policy::Disabled {
                policies.push((id.clone(), policy));
            }
            results.push(json!({"native_id": request.native_id, "id": id, "status": "imported", "policy": Policy::Disabled}));
            specs.push(spec);
        }
        let mut alias_results = Vec::new();
        let mut alias_changes = BTreeMap::new();
        if include_aliases {
            for (alias, target) in &plan.inspection.aliases {
                let state = match resulting.get(target) {
                    _ if snapshot.models.iter().any(|model| model.id == *alias)
                        || ids.contains(alias) =>
                    {
                        "skipped_conflict"
                    }
                    None => "skipped_target_not_imported",
                    Some(id) if snapshot.aliases.entries.get(alias) == Some(id) => "unchanged",
                    Some(_) if snapshot.aliases.entries.contains_key(alias) => "skipped_conflict",
                    Some(_)
                        if snapshot.aliases.entries.len() + alias_changes.len()
                            >= crate::config::MAXIMUM_ALIASES =>
                    {
                        "skipped_limit"
                    }
                    Some(id) => {
                        alias_changes.insert(alias.clone(), id.clone());
                        "added"
                    }
                };
                alias_results.push(
                    json!({"alias": alias, "target": resulting.get(target), "status": state}),
                );
            }
        }
        let verify = plan.inspection.clone();
        tokio::task::spawn_blocking(move || verify.verify())
            .await
            .map_err(|_| interrupted())?
            .map_err(refusal)?;
        let mut generation = snapshot.generation;
        if !specs.is_empty() || !alias_changes.is_empty() {
            let config = self.config.clone().lock_owned().await;
            if config.generation != expected_generation || config.generation != snapshot.generation
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "revision_conflict",
                    "The configuration changed during the import. Preview the import again before applying it.",
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
            for spec in &mut specs {
                spec.revision = next.generation;
                next.models.push(spec.clone());
            }
            if !alias_changes.is_empty() {
                next.aliases.revision = next.generation;
                next.aliases.entries.extend(alias_changes);
            }
            next.validate().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_registration",
                    "An imported model has invalid identity, launch, reservation or operation settings.",
                )
            })?;
            let next = Self::check_config_size(next).await?;
            let parent = self
                .config_path
                .parent()
                .expect("configuration parent")
                .to_owned();
            let mut workers = Vec::with_capacity(specs.len());
            for spec in &next.models[next.models.len() - specs.len()..] {
                let resolved = self.bind_runtime(spec.resolved(&parent)).await?;
                workers.push(Self::create_worker(
                    resolved,
                    None,
                    &self.admission,
                    &self.processes,
                    None,
                )?);
            }
            let outcome = self
                .publish_configuration(config, next, publication::Effect::InsertMany { workers })
                .await?;
            if outcome.warning {
                self.verify_storage().await?;
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "configuration_durability",
                    "The import committed but directory durability is unconfirmed. Refresh before retrying.",
                ));
            }
            generation = outcome.generation;
        }
        for (id, policy) in policies {
            let outcome = self.set_policy(&id, policy, generation).await;
            if let Some(row) = results
                .iter_mut()
                .find(|row| row["id"] == id.as_str() && row["status"] == "imported")
            {
                match outcome {
                    Ok(view) => row["policy"] = json!(view.policy),
                    Err(error) => {
                        row["policy_error"] = json!({"code": error.code, "message": error.message})
                    }
                }
            }
        }
        let generation = self.config.lock().await.generation;
        Ok(json!({
            "schema_version": 1,
            "action": "apply",
            "configuration_generation": generation,
            "models": results,
            "aliases": alias_results,
            "source_modified": false,
            "weights_copied": false,
        }))
    }
}
