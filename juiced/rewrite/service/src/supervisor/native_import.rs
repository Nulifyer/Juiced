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
        "resident_budget_exceeded" => {
            "The estimated resident memory exceeds resident_budget_bytes. Raise the budget or import a smaller quantization."
        }
        "invalid_registration" => {
            "The derived registration has invalid identity, launch, reservation or operation settings."
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

/// A companion resolved in place for one native checkpoint role.
struct Resolved {
    role: String,
    checkpoint: String,
    found: Result<Weights, &'static str>,
}

struct Detected {
    weights: Result<Weights, &'static str>,
    format: Option<Result<models::ModelFormat, models::FormatError>>,
    companions: Vec<Resolved>,
}

/// Native recipes the rewrite runs with another engine name: the retired `hybrid` recipe is
/// llama.cpp on the hybrid-rocm backend.
fn recipe_engine(recipe: Option<&str>) -> Option<(Engine, &'static str, Option<&'static str>)> {
    match recipe? {
        "hybrid" => Some((Engine::LlamaCpp, "llamacpp", Some("hybrid-rocm"))),
        recipe => {
            let engine = Engine::parse(recipe)?;
            Some((engine, engine.recipe(), None))
        }
    }
}

fn detect_candidate(inspection: &Inspection, candidate: &Candidate) -> Detected {
    let definition = candidate.definition.as_ref();
    let engine = recipe_engine(definition.and_then(|definition| definition.recipe.as_deref()))
        .map(|(engine, ..)| engine);
    let mut companions = Vec::new();
    let weights = if engine == Some(Engine::Kokoros) {
        inspection.find_kokoro(candidate).map(|(weights, voices)| {
            let bytes = std::fs::metadata(&voices).map_or(0, |metadata| metadata.len());
            companions.push(Resolved {
                role: "voices".into(),
                checkpoint: "index.json:voices".into(),
                found: Ok(Weights {
                    files: vec![(voices.clone(), bytes)],
                    path: voices,
                    bytes,
                    selection: "kokoro-index",
                    snapshot: weights.snapshot.clone(),
                }),
            });
            weights
        })
    } else {
        inspection
            .find_weights(candidate)
            .map(|weights| match engine {
                Some(Engine::AceStep | Engine::ThinkSound | Engine::Trellis) => {
                    models::generation_directory(weights)
                }
                _ => weights,
            })
    };
    if let Some(definition) = definition {
        let roles: &[&str] = match engine {
            Some(Engine::LlamaCpp) => &["mmproj"],
            Some(Engine::StableDiffusionCpp | Engine::TheNoise) => &["text_encoder", "vae"],
            _ => &[],
        };
        for role in roles {
            if let Some(checkpoint) = definition.checkpoints.get(*role) {
                companions.push(Resolved {
                    role: (*role).to_owned(),
                    checkpoint: checkpoint.clone(),
                    found: inspection.find_checkpoint(checkpoint, definition.checkpoint.as_deref()),
                });
            }
        }
    }
    let format = weights.as_ref().ok().map(|weights| {
        models::detect(&weights.path).and_then(|(format, _)| match (engine, format) {
            (Some(Engine::OpenMoss), models::ModelFormat::Gguf { .. }) => {
                let sidecar = weights
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| weights.path.with_file_name(models::moss_sidecar(name)));
                match sidecar.map(|sidecar| models::moss_family(&sidecar)) {
                    Some(Ok(Some(family))) => Ok(models::ModelFormat::MossGguf { family }),
                    Some(Err(error)) => Err(error),
                    _ => Err(models::FormatError::Invalid("openmoss_sidecar_missing")),
                }
            }
            (
                Some(Engine::TheNoise),
                models::ModelFormat::Diffusion {
                    container: models::Container::Safetensors,
                    architecture,
                    ..
                },
            ) => Ok(models::ModelFormat::TheNoiseDiffusion { architecture }),
            (_, format) => Ok(format),
        })
    });
    Detected {
        weights,
        format,
        companions,
    }
}

/// Native trusts the recipe for the worker type; a file the detector reads as another kind of
/// GGUF still launches with the recipe's engine.
fn recipe_format(engine: Engine, detected: models::ModelFormat) -> models::ModelFormat {
    use models::ModelFormat;
    match (engine, detected) {
        (Engine::StableDiffusionCpp, ModelFormat::Gguf { declared, .. }) => {
            ModelFormat::Diffusion {
                container: models::Container::Gguf,
                architecture: declared.architecture,
                includes_vae: false,
                includes_text_encoder: false,
            }
        }
        (Engine::Asr4all, ModelFormat::Gguf { .. } | ModelFormat::ParakeetGguf {}) => {
            ModelFormat::Asr4allGguf {}
        }
        (Engine::Parakeet, ModelFormat::Gguf { .. } | ModelFormat::Asr4allGguf {}) => {
            ModelFormat::ParakeetGguf {}
        }
        (_, format) => format,
    }
}

/// Places mapped native argument groups into the engine's base arguments: a flag the base
/// already passes takes the native value, any other group is appended in native order.
fn merge_arguments(arguments: &mut Vec<Value>, groups: &[Vec<String>]) {
    for group in groups {
        let position = arguments
            .iter()
            .position(|argument| argument.as_str() == Some(group[0].as_str()));
        match (position, group.len()) {
            (Some(position), 2)
                if arguments
                    .get(position + 1)
                    .and_then(Value::as_str)
                    .is_some_and(|value| {
                        !value.starts_with('-') || value.parse::<f64>().is_ok()
                    }) =>
            {
                arguments[position + 1] = json!(group[1]);
            }
            (Some(_), 1) => {}
            _ => arguments.extend(group.iter().map(|argument| json!(argument))),
        }
    }
}

fn companion_view(companion: &Resolved) -> Value {
    match &companion.found {
        Ok(found) => {
            json!({"role": companion.role, "checkpoint": companion.checkpoint, "found": true, "path": found.path, "bytes": found.bytes})
        }
        Err(code) => {
            json!({"role": companion.role, "checkpoint": companion.checkpoint, "found": false, "reason": code})
        }
    }
}

impl Manager {
    async fn native_import_plan(
        &self,
        source: PathBuf,
        weights: Option<PathBuf>,
        extra_roots: &[PathBuf],
    ) -> Result<Plan, ApiError> {
        let base = self.config.lock().await.clone();
        let mut roots = base.import_roots.clone();
        roots.extend(extra_roots.iter().cloned());
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
                .map(|candidate| detect_candidate(&inspection, candidate))
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
        for (candidate, detected) in inspection.candidates.iter().cloned().zip(detected) {
            let Detected {
                weights,
                format,
                companions,
            } = detected;
            let mut blockers = Vec::new();
            let mut row = serde_json::Map::new();
            let definition = candidate.definition.clone();
            let recipe = definition
                .as_ref()
                .and_then(|definition| definition.recipe.clone());
            let routed = recipe_engine(recipe.as_deref());
            let recipe_engine = routed.map(|(engine, ..)| engine);
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
                Some(Ok(format)) => Some(match recipe_engine {
                    Some(engine) => recipe_format(engine, format),
                    None => format,
                }),
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
            let empty = crate::native_settings::SavedOptions::default();
            let no_rows = BTreeMap::new();
            let image_defaults = definition
                .as_ref()
                .map(|definition| &definition.image_defaults)
                .unwrap_or(&no_rows);
            let registered = definition
                .as_ref()
                .map(|definition| &definition.registered)
                .unwrap_or(&no_rows);
            let saved = candidate.saved.as_ref().unwrap_or(&empty);
            let layers = [
                crate::native_import::Layer {
                    scope: "image_defaults",
                    values: image_defaults
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_ref()))
                        .collect(),
                },
                crate::native_import::Layer {
                    scope: "registration",
                    values: registered
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_ref()))
                        .collect(),
                },
                crate::native_import::Layer {
                    scope: "saved",
                    values: saved.iter().collect(),
                },
            ];
            let labels = definition
                .as_ref()
                .map(|definition| definition.labels.clone())
                .unwrap_or_default();
            let recipe_name =
                routed.map_or(recipe.as_deref().unwrap_or_default(), |(_, name, _)| name);
            let mut mapped = crate::native_import::map_options(
                recipe_engine,
                recipe_name,
                &inspection.config,
                &layers,
                &labels,
            );
            if let Some((_, _, Some(backend))) = routed {
                mapped.backend = Some(backend.to_owned());
            }
            let mut settings = mapped.rows.clone();
            let mut bound: Vec<(&Resolved, crate::artifacts::ArtifactRole)> = Vec::new();
            match recipe_engine {
                Some(Engine::LlamaCpp) => {
                    for companion in &companions {
                        if companion.found.is_ok() {
                            bound.push((companion, crate::artifacts::ArtifactRole::Projector));
                        } else {
                            settings.push(crate::native_import::SettingRow {
                                scope: "registration",
                                key: "checkpoints.mmproj".into(),
                                state: "unsupported",
                                value: Some(companion_view(companion)),
                                target: None,
                                reason: "The vision projector is not downloaded; like native, the model runs for text only.",
                            });
                        }
                    }
                }
                Some(Engine::StableDiffusionCpp) => {
                    let found: Vec<_> = companions
                        .iter()
                        .filter(|companion| companion.found.is_ok())
                        .collect();
                    if found.len() == 2 {
                        for companion in found {
                            bound.push((
                                companion,
                                if companion.role == "vae" {
                                    crate::artifacts::ArtifactRole::Vae
                                } else {
                                    crate::artifacts::ArtifactRole::Encoder
                                },
                            ));
                        }
                    } else if !companions.is_empty() {
                        settings.push(crate::native_import::SettingRow {
                            scope: "registration",
                            key: "checkpoints".into(),
                            state: "unsupported",
                            value: Some(json!(companions.iter().map(companion_view).collect::<Vec<_>>())),
                            target: None,
                            reason: "Native passes the text encoder and VAE only when both are downloaded; the main file is loaded alone.",
                        });
                    }
                }
                Some(Engine::Kokoros) => {
                    for companion in &companions {
                        if companion.found.is_ok() {
                            bound.push((companion, crate::artifacts::ArtifactRole::Voice));
                        }
                    }
                }
                Some(Engine::TheNoise) => {
                    if companions.len() == 2
                        && companions.iter().all(|companion| companion.found.is_ok())
                    {
                        for companion in &companions {
                            bound.push((
                                companion,
                                if companion.role == "vae" {
                                    crate::artifacts::ArtifactRole::Vae
                                } else {
                                    crate::artifacts::ArtifactRole::Encoder
                                },
                            ));
                        }
                    } else {
                        blockers.push(blocker("thenoise_companions_missing"));
                    }
                }
                _ => {}
            }
            if let Some(definition) = &definition {
                for role in &definition.other_checkpoints {
                    if companions.iter().any(|companion| companion.role == *role) {
                        continue;
                    }
                    settings.push(crate::native_import::SettingRow {
                        scope: "registration",
                        key: format!("checkpoints.{role}"),
                        state: "ignored",
                        value: definition.checkpoints.get(role).map(|checkpoint| json!(checkpoint)),
                        target: None,
                        reason: "Native does not pass this checkpoint role to the worker for this recipe.",
                    });
                }
            }
            if recipe_engine == Some(Engine::Kokoros) && bound.is_empty() && weights.is_ok() {
                blockers.push(blocker("companions_required"));
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
                        if mapped.args_backend.as_deref() != Some(choice.backend.as_str())
                            && (mapped.args_backend.is_some()
                                || crate::native_import::has_backend_arguments(
                                    &inspection.config,
                                    recipe_name,
                                    &choice.backend,
                                ))
                        {
                            let preferred = mapped.backend.take();
                            let later = settings.split_off(mapped.rows.len().min(settings.len()));
                            mapped = crate::native_import::map_options_for(
                                recipe_engine,
                                recipe_name,
                                &inspection.config,
                                &layers,
                                &labels,
                                Some(&choice.backend),
                            );
                            mapped.backend = preferred;
                            settings = mapped.rows.clone();
                            settings.extend(later);
                            settings.push(crate::native_import::SettingRow {
                                scope: "global",
                                key: format!("{}.{}_args", recipe_name.replace('-', ""), crate::native_import::display_key(&choice.backend)),
                                state: if mapped.args_backend.is_some() { "mapped" } else { "default" },
                                value: None,
                                target: Some("arguments"),
                                reason: if mapped.args_backend.is_some() {
                                    "The arguments saved for the backend this host runs replace the engine's args."
                                } else {
                                    "This host runs another backend than the native preference, so the engine's args apply."
                                },
                            });
                        }
                        if mapped
                            .backend
                            .as_deref()
                            .is_some_and(|native| native != choice.backend)
                        {
                            let before = mapped.arguments.len();
                            mapped.arguments.retain(|group| {
                                !matches!(
                                    group[0].as_str(),
                                    "--device"
                                        | "-dev"
                                        | crate::hybrid::DEVICE_FLAG
                                        | crate::hybrid::MEMORY_FLAG
                                        | crate::hybrid::MIN_PROMPT_FLAG
                                )
                            });
                            if mapped.arguments.len() != before {
                                settings.push(crate::native_import::SettingRow {
                                    scope: "saved",
                                    key: "llamacpp_device".into(),
                                    state: "ignored",
                                    value: None,
                                    target: None,
                                    reason: "The native device belongs to a backend this host does not use; the engine default device applies.",
                                });
                            }
                        }
                        let companion_bytes: u64 = bound
                            .iter()
                            .filter_map(|(companion, _)| companion.found.as_ref().ok())
                            .map(|found| found.bytes)
                            .sum();
                        let bytes = found.bytes.saturating_add(companion_bytes);
                        let mut llama_defaults = None;
                        let estimate = match format {
                            models::ModelFormat::Gguf { declared, shape } => {
                                let slots = mapped.options.parallel_slots.map_or(1, u64::from);
                                let defaults = crate::engines::memory::LlamaDefaults::for_model(
                                    shape,
                                    declared.architecture.as_deref(),
                                    slots,
                                );
                                let mut planned: Vec<String> =
                                    mapped.arguments.iter().flatten().cloned().collect();
                                defaults.apply(&mut planned);
                                let plan = |context: u64| {
                                    crate::engines::memory::Plan::from_arguments(
                                        &planned, context, slots, choice.gpu,
                                    )
                                };
                                let estimate_at = |context: u64| {
                                    models::estimate_with(
                                        format,
                                        found.bytes,
                                        companion_bytes,
                                        Some(&plan(context)),
                                    )
                                };
                                let share = self.auto_fit_share(&candidate.public_name);
                                let explicit = mapped.options.context_tokens.map(u64::from);
                                let limit = explicit.or(declared.training_context_tokens);
                                let context = if explicit.is_some_and(|context| {
                                    estimate_at(context).resident_bytes <= share
                                }) {
                                    explicit.unwrap_or_default()
                                } else {
                                    crate::engines::memory::auto_context(limit, share, |context| {
                                        Some(estimate_at(context).resident_bytes)
                                    })
                                };
                                if let Some(saved) = explicit
                                    && context < saved
                                {
                                    settings.push(crate::native_import::SettingRow {
                                        scope: "saved",
                                        key: "ctx_size".into(),
                                        state: "mapped",
                                        value: Some(json!(context)),
                                        target: Some("context_tokens"),
                                        reason: "The saved context does not fit the resident budget, so the largest context that fits applies.",
                                    });
                                }
                                llama_defaults = Some(defaults);
                                estimate_at(context)
                            }
                            _ => models::estimate(format, bytes, None),
                        };
                        let mut companion_arguments: Vec<String> = Vec::new();
                        for (companion, role) in &bound {
                            let flag = match (engine, role) {
                                (Engine::LlamaCpp, _) => "--mmproj",
                                (Engine::Kokoros, _) => "--data",
                                (Engine::TheNoise, crate::artifacts::ArtifactRole::Encoder) => {
                                    "--text-encoder"
                                }
                                (_, crate::artifacts::ArtifactRole::Vae) => "--vae",
                                _ => "--llm",
                            };
                            companion_arguments.extend([
                                flag.to_owned(),
                                crate::runtime_arguments::member_placeholder(&companion.role),
                            ]);
                        }
                        if engine == Engine::LlamaCpp && !bound.is_empty() && !choice.gpu {
                            companion_arguments.push("--no-mmproj-offload".into());
                        }
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
                            companion_arguments: if engine == Engine::LlamaCpp {
                                &[]
                            } else {
                                &companion_arguments
                            },
                            llama: None,
                        });
                        let profile = &mut value["execution_profile"];
                        if let Some(arguments) = profile["args"].as_array_mut() {
                            if engine == Engine::StableDiffusionCpp {
                                let flag = if bound.len() == 2 {
                                    "--diffusion-model"
                                } else {
                                    "--model"
                                };
                                if let Some(position) = arguments.iter().position(|argument| {
                                    argument == "--model" || argument == "--diffusion-model"
                                }) {
                                    arguments[position] = json!(flag);
                                }
                            }
                            if engine == Engine::LlamaCpp {
                                arguments.extend(
                                    companion_arguments.iter().map(|argument| json!(argument)),
                                );
                            }
                            merge_arguments(arguments, &mapped.arguments);
                            if let Some(defaults) = &llama_defaults {
                                let mut texts: Vec<String> = arguments
                                    .iter()
                                    .filter_map(|argument| argument.as_str().map(str::to_owned))
                                    .collect();
                                defaults.apply(&mut texts);
                                *arguments = texts.into_iter().map(Value::String).collect();
                            }
                        }
                        if matches!(format, models::ModelFormat::Gguf { .. }) {
                            let options = &mut profile["options"];
                            options["context_tokens"] = json!(estimate.context_tokens);
                            if let Some(layers) = mapped.options.gpu_layers {
                                options["gpu_layers"] = json!(if choice.gpu { layers } else { 0 });
                            }
                            if let Some(threads) = mapped.options.threads {
                                options["threads"] = json!(threads);
                            }
                            if let Some(slots) = mapped.options.parallel_slots {
                                options["parallel_slots"] = json!(slots);
                                value["max_inflight"] = json!(slots);
                            }
                        }
                        if !bound.is_empty() {
                            value["companions"] = json!(
                                bound
                                    .iter()
                                    .filter_map(|(companion, role)| {
                                        let found = companion.found.as_ref().ok()?;
                                        Some(json!({"name": companion.role, "role": role, "path": found.path}))
                                    })
                                    .collect::<Vec<_>>()
                            );
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
                        match Self::trial_registration(&base, &value) {
                            Ok(()) => registration = Some(value),
                            Err(code) => blockers.push(blocker(code)),
                        }
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
                Some("hot") => Policy::Hot,
                Some("on-demand") => Policy::OnDemand,
                _ => Policy::Disabled,
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
            row.insert("labels".into(), json!(labels));
            row.insert("selected_profile".into(), json!(candidate.selected_profile));
            row.insert(
                "weights".into(),
                match &weights {
                    Ok(found) => json!({"found": true, "path": found.path, "bytes": found.bytes, "files": found.files.len(), "selection": found.selection, "snapshot": found.snapshot}),
                    Err(code) => json!({"found": false, "reason": code}),
                },
            );
            row.insert(
                "companions".into(),
                json!(
                    companions
                        .iter()
                        .map(|companion| {
                            let mut view = companion_view(companion);
                            view["bound"] =
                                json!(bound.iter().any(|(held, _)| std::ptr::eq(*held, companion)));
                            view
                        })
                        .collect::<Vec<_>>()
                ),
            );
            row.insert("format".into(), json!(format));
            row.insert("engine".into(), engine_view);
            row.insert("options".into(), json!(mapped.options));
            row.insert(
                "argument_flags".into(),
                json!(
                    mapped
                        .arguments
                        .iter()
                        .map(|group| &group[0])
                        .collect::<Vec<_>>()
                ),
            );
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
            row.insert("adopted_policy".into(), json!(suggested));
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

    fn trial_registration(base: &Config, value: &Value) -> Result<(), &'static str> {
        let crate::config::Registration(spec) =
            serde_json::from_value(value.clone()).map_err(|_| "invalid_registration")?;
        let mut trial = base.clone();
        trial.models.clear();
        trial.aliases = Default::default();
        let exceeds = trial.admission.memory.mode == crate::admission::MemoryMode::Legacy
            && spec.resident_bytes > trial.resident_budget_bytes;
        trial.models.push(spec);
        match trial.validate() {
            Ok(()) => Ok(()),
            Err(_) if exceeds => Err("resident_budget_exceeded"),
            Err(_) => Err("invalid_registration"),
        }
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
                self.native_import_preview(source, weights, &operator_roots())
                    .await
            }
            NativeImportRequest::Apply {
                source,
                weights,
                models,
                aliases,
                expected_generation,
            } => {
                self.native_import_apply(
                    source,
                    weights,
                    models,
                    aliases,
                    expected_generation,
                    &operator_roots(),
                )
                .await
            }
        }
    }

    pub(super) async fn native_import_preview(
        self: &Arc<Self>,
        source: PathBuf,
        weights: Option<PathBuf>,
        extra_roots: &[PathBuf],
    ) -> Result<Value, ApiError> {
        let plan = self
            .native_import_plan(source, weights, extra_roots)
            .await?;
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
            "weights_roots": plan.inspection.weights_roots,
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

    pub(super) async fn native_import_apply(
        self: &Arc<Self>,
        source: PathBuf,
        weights: Option<PathBuf>,
        requested: Vec<NativeImportModel>,
        include_aliases: bool,
        expected_generation: u64,
        extra_roots: &[PathBuf],
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
        let plan = self
            .native_import_plan(source, weights, extra_roots)
            .await?;
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

/// Read roots the operator granted for native adoption also serve Library › Import.
fn operator_roots() -> Vec<PathBuf> {
    super::native_adoption::from_env()
        .map(|(_, roots)| roots)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_registration_reports_budget_and_shape_failures_per_model() {
        let base: Config = serde_json::from_value(json!({
            "schema_version": 2, "resident_budget_bytes": 1000, "startup_timeout_ms": 1,
            "request_timeout_ms": 1, "models": [], "aliases": {"revision": 1, "entries": {"other": "x"}},
        }))
        .unwrap();
        let registration = |resident: u64, operations: Value| {
            json!({
                "id": "imported", "model_file": "/weights/model.gguf",
                "execution_profile": {"schema_version": 1, "adapter": "llama-cpp", "implementation_id": "llama-cpp",
                    "command": "/engine/llama-server", "args": ["--port", "{port}", "--model", "{model}"],
                    "options": {}, "placement": "unresolved"},
                "resident_bytes": resident, "max_inflight": 1, "operations": operations, "policy": "disabled",
            })
        };
        let chat = json!(["chat/completions"]);
        assert_eq!(
            Manager::trial_registration(&base, &registration(1000, chat.clone())),
            Ok(())
        );
        assert_eq!(
            Manager::trial_registration(&base, &registration(1001, chat)),
            Err("resident_budget_exceeded")
        );
        assert_eq!(
            Manager::trial_registration(&base, &registration(10, json!([]))),
            Err("invalid_registration")
        );
        assert_eq!(
            Manager::trial_registration(&base, &json!({"id": "x"})),
            Err("invalid_registration")
        );
    }
}
