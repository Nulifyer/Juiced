use super::{Manager, Worker};
use crate::{
    ApiError,
    config::{ModelSpec, Policy, Provider},
    engines::{
        memory::{self, Breakdown, CacheType, FitDevice, Plan},
        models::{self, ModelFormat},
    },
    profiles::{DeclaredContext, LaunchOptions, ModelShape},
    runtime_bundle::{BundleSpec, FileRole},
};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, atomic::Ordering},
    time::{Duration, Instant},
};

const FIT_EXECUTABLE: &str = "llama-fit-params";
const FIT_TIMEOUT: Duration = Duration::from_secs(20);
const FIT_OUTPUT_LIMIT: usize = 64 * 1024;
const FIT_CACHE_ENTRIES: usize = 256;
const GPU_LIBRARIES: [&str; 4] = [
    "libggml-vulkan",
    "libggml-hip",
    "libggml-cuda",
    "libggml-metal",
];

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitQuery {
    pub context_tokens: Option<u32>,
    pub gpu_layers: Option<u32>,
    pub kv_cache_type: Option<String>,
    pub parallel_slots: Option<u32>,
}

impl FitQuery {
    fn is_draft(&self) -> bool {
        self.context_tokens.is_some()
            || self.gpu_layers.is_some()
            || self.kv_cache_type.is_some()
            || self.parallel_slots.is_some()
    }
}

fn fit_cache() -> &'static Mutex<HashMap<String, Vec<FitDevice>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<FitDevice>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn fit_gate() -> &'static tokio::sync::Semaphore {
    static GATE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Semaphore::new(1))
}

fn read_shape(path: &Path) -> Option<(DeclaredContext, ModelShape)> {
    let mut file = std::fs::File::open(path).ok()?;
    crate::profiles::read_gguf_shape(&mut file).ok()
}

fn file_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

pub(super) fn bundle_gpu(bundle: Option<&BundleSpec>) -> Option<bool> {
    bundle.map(|bundle| {
        bundle.files.iter().any(|file| {
            file.path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| GPU_LIBRARIES.iter().any(|prefix| name.starts_with(prefix)))
        })
    })
}

fn projector_bytes(spec: &ModelSpec, worker: Option<&Worker>) -> u64 {
    let in_place: u64 = spec
        .companions
        .iter()
        .filter(|companion| companion.role == crate::artifacts::ArtifactRole::Projector)
        .map(|companion| file_bytes(&companion.path))
        .sum();
    let managed: u64 = worker
        .and_then(|worker| worker._artifact.as_ref())
        .map(|artifact| {
            artifact
                .companions()
                .into_iter()
                .filter(|(_, role, _)| *role == crate::artifacts::ArtifactRole::Projector)
                .map(|(_, _, path)| file_bytes(path))
                .sum()
        })
        .unwrap_or(0);
    in_place.saturating_add(managed)
}

/// The memory plan a llama.cpp registration launches with: its resolved arguments, context
/// (trained when unset), slots and whether the runtime offloads to a GPU.
fn launch_plan(
    spec: &ModelSpec,
    args: &[String],
    options: &LaunchOptions,
    declared: &DeclaredContext,
) -> Plan {
    let context = options
        .context_tokens
        .map(u64::from)
        .or(declared.training_context_tokens)
        .unwrap_or(models::UNKNOWN_CONTEXT_TOKENS);
    let slots = options
        .parallel_slots
        .map_or(u64::from(spec.max_inflight), u64::from);
    let gpu = options.gpu_layers != Some(0)
        && bundle_gpu(spec.execution_profile.runtime_bundle()).unwrap_or(true);
    Plan::from_arguments(args, context, slots, gpu)
}

/// The automatic reservation for an existing llama.cpp registration, or `None` when the model
/// is not a readable GGUF.
pub(super) fn formula_reservation(spec: &ModelSpec, worker: Option<&Worker>) -> Option<(u64, u64)> {
    if spec.execution_profile.adapter != Provider::LlamaCpp {
        return None;
    }
    let (template, options) = spec.execution_profile.resolved_arguments().ok()?;
    let options = options?;
    let (declared, shape) = read_shape(&spec.model_file)?;
    let format = ModelFormat::Gguf {
        declared: declared.clone(),
        shape: Box::new(shape),
    };
    let model = file_bytes(&spec.model_file);
    let projector = projector_bytes(spec, worker);
    let plan = launch_plan(spec, template.arguments(), &options, &declared);
    let legacy = models::legacy_resident_bytes(
        &format,
        model.saturating_add(projector),
        plan.context_tokens,
    );
    let estimate = models::estimate_with(&format, model, projector, Some(&plan));
    estimate
        .breakdown
        .map(|_| (legacy, estimate.resident_bytes))
}

/// Replaces a reservation that earlier releases generated with the old formula, so existing
/// registrations get the corrected charge without an operator edit. Explicit values stay.
pub(super) fn refresh_reservation(mut spec: ModelSpec) -> ModelSpec {
    if spec.execution_profile.native_model_id().is_none()
        && let Some((legacy, corrected)) = formula_reservation(&spec, None)
        && spec.resident_bytes == legacy
        && corrected > 0
    {
        spec.resident_bytes = corrected;
    }
    spec
}

fn fit_key(bundle: &BundleSpec, model: &Path, args: &[String]) -> String {
    let metadata = std::fs::metadata(model).ok();
    let modified = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| time.as_nanos());
    let entry = bundle
        .files
        .iter()
        .find(|file| file.path == bundle.entrypoint)
        .map(|file| file.sha256.as_str());
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&(
            "juiced-fit-params-v1",
            &bundle.root,
            entry,
            model,
            metadata.as_ref().map(std::fs::Metadata::len),
            modified,
            args,
        ))
        .unwrap_or_default(),
    );
    format!("{:x}", digest.finalize())
}

/// The bundle's `llama-fit-params` beside its `llama-server`, when the bundle records it as an
/// executable and is not the hybrid NPU engine.
fn fit_executable(bundle: &BundleSpec) -> Option<(PathBuf, String)> {
    let directory = bundle.entrypoint.parent().unwrap_or(Path::new(""));
    if bundle
        .files
        .iter()
        .any(|file| file.path == directory.join(crate::hybrid::PLUGIN))
    {
        return None;
    }
    let relative = directory.join(FIT_EXECUTABLE);
    bundle
        .files
        .iter()
        .find(|file| file.path == relative && file.role == FileRole::Executable)
        .map(|file| (bundle.root.join(&relative), file.sha256.clone()))
}

/// Runs the runtime's own `llama-fit-params` for the planned launch without loading weights.
pub(super) async fn fit_probe(
    bundle: &BundleSpec,
    model: &Path,
    launch_args: &[String],
) -> Result<Vec<FitDevice>, &'static str> {
    let (executable, sha256) = fit_executable(bundle).ok_or("fit_params_unavailable")?;
    let model_text = model.to_str().ok_or("fit_params_path_encoding")?;
    let args = memory::fit_arguments(launch_args, model_text);
    let key = fit_key(bundle, model, &args);
    if let Some(devices) = fit_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
    {
        return Ok(devices.clone());
    }
    let _permit = fit_gate()
        .acquire()
        .await
        .map_err(|_| "fit_params_unavailable")?;
    let verify = executable.clone();
    let verified = tokio::task::spawn_blocking(move || {
        crate::identity::file_digest(&verify, Instant::now() + FIT_TIMEOUT)
            .is_ok_and(|digest| digest == sha256)
    })
    .await
    .unwrap_or(false);
    if !verified {
        return Err("fit_params_changed");
    }
    let library_path = std::env::join_paths(
        bundle
            .library_directories
            .iter()
            .map(|directory| bundle.root.join(directory)),
    )
    .map_err(|_| "fit_params_path_encoding")?;
    let mut command = tokio::process::Command::new(&executable);
    crate::profiles::CapturedEnvironment::capture(None)
        .map_err(|_| "fit_params_environment")?
        .apply(&mut command);
    #[cfg(target_os = "linux")]
    command.env("LD_LIBRARY_PATH", library_path);
    #[cfg(target_os = "macos")]
    command.env("DYLD_LIBRARY_PATH", library_path);
    #[cfg(windows)]
    {
        let mut paths: Vec<_> = std::env::split_paths(&library_path).collect();
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env(
            "PATH",
            std::env::join_paths(paths).map_err(|_| "fit_params_path_encoding")?,
        );
    }
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| "fit_params_spawn")?;
    let mut stdout = child.stdout.take().ok_or("fit_params_spawn")?;
    let read = async {
        use tokio::io::AsyncReadExt;
        let mut output = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stdout.read(&mut buffer).await.ok()?;
            if count == 0 {
                break;
            }
            if output.len() + count > FIT_OUTPUT_LIMIT {
                return None;
            }
            output.extend_from_slice(&buffer[..count]);
        }
        child.wait().await.ok()?;
        Some(output)
    };
    let output = tokio::time::timeout(FIT_TIMEOUT, read)
        .await
        .map_err(|_| "fit_params_timeout")?
        .ok_or("fit_params_failed")?;
    let devices =
        memory::parse_fit_print(&String::from_utf8_lossy(&output)).ok_or("fit_params_failed")?;
    let mut cache = fit_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache.len() >= FIT_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(key, devices.clone());
    Ok(devices)
}

impl Manager {
    /// Budget left for a new model's automatic context: the resident budget minus what models
    /// kept loaded (Hot) already reserve.
    pub(super) fn auto_fit_share(&self, exclude: &str) -> u64 {
        let hot: u64 = self
            .workers
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .filter(|worker| worker.spec.id != exclude)
            .filter(|worker| {
                worker
                    .observation
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .is_some_and(|view| view.policy == Policy::Hot)
            })
            .map(|worker| worker.spec.resident_bytes)
            .fold(0u64, u64::saturating_add);
        self.budget.limit.saturating_sub(hot)
    }

    /// Budget capacity, and what is free for this worker, counting its own reservation as free
    /// while it is loaded.
    pub(super) fn budget_for(&self, worker: &Worker) -> (u64, u64, bool) {
        let loaded = worker
            .observation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|view| view.loaded || view.state == "loading");
        if let Some(snapshot) = self.admission.memory_snapshot() {
            let own = if loaded {
                worker
                    .spec
                    .resource_profile
                    .as_ref()
                    .map_or(0, |profile| profile.resident_bytes)
            } else {
                0
            };
            let capacity = snapshot.capacity_bytes.unwrap_or(0);
            let available = capacity
                .saturating_sub(snapshot.reserved_bytes)
                .saturating_sub(snapshot.essential_headroom_bytes)
                .saturating_add(own);
            return (capacity, available, loaded);
        }
        let used = self.budget.used.load(Ordering::Acquire);
        let own = if loaded {
            worker.spec.resident_bytes
        } else {
            0
        };
        (
            self.budget.limit,
            self.budget.limit.saturating_sub(used.saturating_sub(own)),
            loaded,
        )
    }

    pub async fn model_fit(&self, id: &str, query: FitQuery) -> Result<Value, ApiError> {
        let worker = self.worker(id)?;
        let revision = worker.state.lock().await.revision;
        self.ensure_registered(&worker)?;
        let spec = &worker.spec;
        let invalid = |code: &'static str, message: &str| {
            ApiError::new(StatusCode::BAD_REQUEST, code, message.to_owned())
        };
        let (budget_bytes, available_bytes, loaded) = self.budget_for(&worker);
        if spec.execution_profile.adapter != Provider::LlamaCpp {
            if query.is_draft() {
                return Err(invalid(
                    "fit_options_unsupported",
                    "Context, GPU layers, KV cache type and slots apply only to llama.cpp models.",
                ));
            }
            let total = spec.resident_bytes;
            return Ok(json!({
                "schema_version": 1, "model": id, "revision": revision, "source": "reservation",
                "breakdown": {"weights_bytes": total, "context_bytes": 0, "compute_bytes": 0, "projector_bytes": 0, "draft_bytes": 0, "other_bytes": 0},
                "total_bytes": total, "reservation_bytes": spec.resident_bytes,
                "budget_bytes": budget_bytes, "available_bytes": available_bytes, "loaded": loaded,
                "fits": total <= available_bytes, "largest_context_that_fits": null, "trained_context": null,
                "plan": null, "devices": [], "draft": false,
                "warnings": ["Only llama.cpp models have a per-setting memory layout; this is the saved reservation."],
            }));
        }
        let (template, saved) = spec
            .execution_profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?;
        let saved = saved.unwrap_or_default();
        let draft = LaunchOptions {
            context_tokens: query.context_tokens.or(saved.context_tokens),
            gpu_layers: query.gpu_layers.or(saved.gpu_layers),
            parallel_slots: query.parallel_slots.or(saved.parallel_slots),
            ..saved
        };
        let resolved = draft
            .resolve(template.arguments())
            .map_err(|error| invalid(error.code, &error.message))?;
        let mut args = resolved.args;
        if let Some(kind) = &query.kv_cache_type {
            let kind = CacheType::parse(kind).ok_or_else(|| {
                invalid(
                    "invalid_kv_cache_type",
                    "KV cache type must be f32, f16, bf16, q8_0, q4_0, q4_1, q5_0, q5_1 or iq4_nl.",
                )
            })?;
            args = without_flags(&args, &["-ctk", "--cache-type-k", "-ctv", "--cache-type-v"]);
            args.extend([
                "--cache-type-k".into(),
                kind.name().into(),
                "--cache-type-v".into(),
                kind.name().into(),
            ]);
        }
        let model_file = spec.model_file.clone();
        let (declared, shape) = tokio::task::spawn_blocking(move || read_shape(&model_file))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "model_metadata_unavailable",
                    "The model file is not a readable GGUF, so no memory layout is available.",
                )
            })?;
        let model_bytes = file_bytes(&spec.model_file);
        let projector = projector_bytes(spec, Some(&worker));
        let plan = launch_plan(spec, &args, &resolved.applied, &declared);
        let architecture = declared.architecture.as_deref();
        let formula = |plan: &Plan| {
            memory::llama(&shape, architecture, model_bytes, projector, plan).unwrap_or(Breakdown {
                weights_bytes: model_bytes,
                context_bytes: model_bytes / 4,
                compute_bytes: models::WORKSPACE_ALLOWANCE_BYTES,
                projector_bytes: projector,
                ..Breakdown::default()
            })
        };
        let estimate = formula(&plan);
        let mut warnings = Vec::new();
        let (breakdown, source, devices) = match spec.execution_profile.runtime_bundle() {
            Some(bundle) => match fit_probe(bundle, &spec.model_file, &args).await {
                Ok(devices) => (
                    memory::with_fit(&estimate, &devices),
                    "llama-fit-params",
                    devices,
                ),
                Err(code) => {
                    warnings.push(json!({"code": code, "message": "The runtime's llama-fit-params did not report a layout; the metadata estimate applies."}));
                    (estimate, "estimate", Vec::new())
                }
            },
            None => (estimate, "estimate", Vec::new()),
        };
        let total_bytes = breakdown.total();
        let offset = i128::from(total_bytes) - i128::from(estimate.total());
        let trained = declared.training_context_tokens;
        let total_at = |context: u64| {
            let mut at = plan.clone();
            at.context_tokens = context;
            u64::try_from(i128::from(formula(&at).total()) + offset).ok()
        };
        let largest = memory::auto_context(trained, available_bytes, total_at);
        let largest = total_at(largest)
            .is_some_and(|total| total <= available_bytes)
            .then_some(largest);
        if trained.is_some_and(|trained| plan.context_tokens > trained) {
            warnings.push(json!({"code": "context_exceeds_trained", "message": "The context is longer than the model was trained for."}));
        }
        if plan.mtp && plan.slots > 1 {
            warnings.push(json!({"code": "mtp_requires_single_slot", "message": "MTP drafting is meant for one slot; with more slots it can slow decoding."}));
        }
        if plan.mtp && !memory::has_mtp_head(&shape) {
            warnings.push(json!({"code": "mtp_head_missing", "message": "--spec-type draft-mtp is set but the GGUF has no MTP head."}));
        }
        if !matches!(
            plan.cache_type_v,
            CacheType::F16 | CacheType::F32 | CacheType::Bf16
        ) && plan.flash_attention == Some(false)
        {
            warnings.push(json!({"code": "quantized_v_requires_flash_attention", "message": "A quantized V cache needs flash attention."}));
        }
        if !query.is_draft() && spec.resident_bytes != total_bytes {
            warnings.push(json!({"code": "reservation_differs", "message": "The saved reservation differs from this layout; saving the settings updates it.", "reservation_bytes": spec.resident_bytes}));
        }
        Ok(json!({
            "schema_version": 1, "model": id, "revision": revision, "source": source,
            "breakdown": breakdown, "total_bytes": total_bytes, "reservation_bytes": spec.resident_bytes,
            "budget_bytes": budget_bytes, "available_bytes": available_bytes, "loaded": loaded,
            "fits": total_bytes <= available_bytes, "largest_context_that_fits": largest,
            "trained_context": trained, "plan": plan, "devices": devices, "draft": query.is_draft(),
            "warnings": warnings,
        }))
    }
}

fn without_flags(args: &[String], flags: &[&str]) -> Vec<String> {
    let mut kept = Vec::with_capacity(args.len());
    let mut position = 0;
    while position < args.len() {
        let (flag, inline) = args[position]
            .split_once('=')
            .map_or((args[position].as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        if flags.contains(&flag) {
            position += if inline.is_some() { 1 } else { 2 };
        } else {
            kept.push(args[position].clone());
            position += 1;
        }
    }
    kept
}

/// One launch setting as saved, as passed to the running engine, and as the engine reports it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Settings {
    context: Option<u64>,
    gpu_layers: Option<u64>,
    slots: Option<u64>,
    threads: Option<u64>,
    kv_cache_type: Option<String>,
    flash_attention: Option<String>,
    mtp: Option<bool>,
    thinking: Option<String>,
}

fn flag_value<'a>(args: &'a [String], flags: &[&str]) -> Option<&'a str> {
    let mut found = None;
    let mut position = 0;
    while position < args.len() {
        let (flag, inline) = args[position]
            .split_once('=')
            .map_or((args[position].as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        if flags.contains(&flag) {
            found = inline.or_else(|| args.get(position + 1).map(String::as_str));
            position += if inline.is_some() { 1 } else { 2 };
        } else {
            position += 1;
        }
    }
    found
}

impl Settings {
    fn read(args: &[String], options: Option<&LaunchOptions>) -> Self {
        let key = flag_value(args, &["-ctk", "--cache-type-k"]);
        let value = flag_value(args, &["-ctv", "--cache-type-v"]);
        let kv_cache_type = match (key, value) {
            (None, None) => None,
            (key, value) if key == value => key.map(str::to_owned),
            (key, value) => Some(format!(
                "{}/{}",
                key.unwrap_or("f16"),
                value.unwrap_or("f16")
            )),
        };
        let kwargs = flag_value(args, &["--chat-template-kwargs"])
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        let thinking = flag_value(args, &["-rea", "--reasoning"])
            .map(str::to_owned)
            .or_else(|| {
                (flag_value(args, &["--reasoning-budget"]) == Some("0")).then(|| "off".to_owned())
            })
            .or_else(|| {
                kwargs
                    .as_ref()
                    .and_then(|kwargs| kwargs.get("enable_thinking"))
                    .and_then(Value::as_bool)
                    .map(|enabled| if enabled { "on" } else { "off" }.to_owned())
            });
        Self {
            context: options
                .and_then(|options| options.context_tokens)
                .map(u64::from),
            gpu_layers: options
                .and_then(|options| options.gpu_layers)
                .map(u64::from),
            slots: options
                .and_then(|options| options.parallel_slots)
                .map(u64::from),
            threads: options.and_then(|options| options.threads).map(u64::from),
            kv_cache_type,
            flash_attention: flag_value(args, &["-fa", "--flash-attn"]).map(str::to_owned),
            mtp: flag_value(args, &["--spec-type"]).map(|kind| kind == "draft-mtp"),
            thinking,
        }
    }
}

struct Automatic {
    blocks: Option<u64>,
    mtp_head: bool,
    context: Option<u64>,
}

fn field(
    saved: Option<Value>,
    launched: Option<Option<Value>>,
    reported: Option<Value>,
    engine_default: Value,
    automatic: bool,
    model_default: bool,
) -> Value {
    let restart_needed = launched.as_ref().is_some_and(|launched| *launched != saved);
    let (effective, source) = if let Some(reported) = &reported {
        (reported.clone(), "reported")
    } else {
        let current = match &launched {
            Some(launched) => launched.clone(),
            None => saved.clone(),
        };
        match current {
            None => (engine_default, "engine"),
            Some(value) if model_default => (value, "model"),
            Some(value) if automatic => (value, "auto"),
            Some(value) => (value, "saved"),
        }
    };
    json!({
        "saved": saved,
        "launched": launched.flatten(),
        "effective": effective,
        "source": source,
        "restart_needed": restart_needed,
    })
}

fn settings_fields(
    saved: &Settings,
    launched: Option<&Settings>,
    reported: (Option<u64>, Option<u64>),
    automatic: &Automatic,
) -> Value {
    let number = |value: Option<u64>| value.map(Value::from);
    let text = |value: &Option<String>| value.clone().map(Value::from);
    let current = launched.unwrap_or(saved);
    json!({
        "context": field(
            number(saved.context), launched.map(|l| number(l.context)), number(reported.0),
            json!("trained"),
            automatic.context.is_some() && current.context == automatic.context, false,
        ),
        "gpu_layers": field(
            number(saved.gpu_layers), launched.map(|l| number(l.gpu_layers)), None, json!("auto"),
            current.gpu_layers.is_some_and(|layers| layers >= 999 || automatic.blocks.is_some_and(|blocks| layers >= blocks)), false,
        ),
        "slots": field(
            number(saved.slots), launched.map(|l| number(l.slots)), number(reported.1), json!("auto"),
            current.slots == Some(1), false,
        ),
        "threads": field(
            number(saved.threads), launched.map(|l| number(l.threads)), None, json!("auto"), false, false,
        ),
        "kv_cache_type": field(
            text(&saved.kv_cache_type), launched.map(|l| text(&l.kv_cache_type)), None, json!("f16"), false, false,
        ),
        "flash_attention": field(
            text(&saved.flash_attention), launched.map(|l| text(&l.flash_attention)), None, json!("auto"), false, false,
        ),
        "mtp": field(
            saved.mtp.map(Value::from), launched.map(|l| l.mtp.map(Value::from)), None, json!(false),
            false, current.mtp == Some(true) && automatic.mtp_head,
        ),
        "thinking": field(
            text(&saved.thinking), launched.map(|l| text(&l.thinking)), None, json!("auto"), false, false,
        ),
    })
}

/// Speculative decoding counters from llama-server's Prometheus `/metrics`.
pub(super) fn parse_mtp_metrics(text: &str) -> Option<Value> {
    let counter = |name: &str| {
        text.lines()
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| {
                let (key, value) = line.rsplit_once(' ')?;
                (key == name)
                    .then(|| value.trim().parse::<f64>().ok())
                    .flatten()
            })
    };
    let drafted = counter("llamacpp:spec_decode_num_draft_tokens_total")?;
    let accepted = counter("llamacpp:spec_decode_num_accepted_tokens_total")?;
    let drafts = counter("llamacpp:spec_decode_num_drafts_total");
    Some(json!({
        "source": "llamacpp:/metrics",
        "draft_tokens": drafted as u64,
        "accepted_tokens": accepted as u64,
        "drafts": drafts.map(|drafts| drafts as u64),
        "acceptance_rate": (drafted > 0.0).then(|| accepted / drafted),
        "accepted_per_draft": drafts.filter(|drafts| *drafts > 0.0).map(|drafts| accepted / drafts),
    }))
}

impl Manager {
    async fn engine_metrics(&self, base: &str) -> Option<String> {
        let mut response = self
            .client
            .get(format!("{base}/metrics"))
            .timeout(Duration::from_millis(500))
            .send()
            .await
            .ok()
            .filter(|response| response.status().is_success())?;
        let mut bytes = Vec::with_capacity(4096);
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).ok()
    }

    /// Per-setting provenance for the model inspector (`fields`) and MTP acceptance.
    pub(super) async fn launch_provenance(&self, id: &str) -> Option<(Value, Value)> {
        let worker = self.worker(id).ok()?;
        let spec = &worker.spec;
        if spec.execution_profile.adapter != Provider::LlamaCpp {
            return None;
        }
        let (template, options) = spec.execution_profile.resolved_arguments().ok()?;
        let saved = Settings::read(template.arguments(), options.as_ref());
        let running = {
            let state = worker.state.lock().await;
            state.running.as_ref().map(|run| {
                (
                    Settings::read(run.plan.arguments(), run.plan.effective()),
                    run.base.clone(),
                    memory::has_flag(run.plan.arguments(), "--metrics"),
                )
            })
        };
        let model_file = spec.model_file.clone();
        let shape = tokio::task::spawn_blocking(move || read_shape(&model_file))
            .await
            .ok()
            .flatten();
        let automatic = match &shape {
            Some((declared, shape)) => {
                let model = file_bytes(&spec.model_file);
                let projector = projector_bytes(spec, Some(&worker));
                let gpu = bundle_gpu(spec.execution_profile.runtime_bundle()).unwrap_or(true);
                let slots = saved.slots.unwrap_or(1);
                let mut args = template.arguments().to_vec();
                memory::LlamaDefaults::for_model(shape, declared.architecture.as_deref(), slots)
                    .apply(&mut args);
                let share = self.auto_fit_share(id);
                let base = Plan::from_arguments(&args, 1, slots, gpu);
                let context =
                    memory::auto_context(declared.training_context_tokens, share, |context| {
                        let plan = Plan {
                            context_tokens: context,
                            ..base.clone()
                        };
                        memory::llama(
                            shape,
                            declared.architecture.as_deref(),
                            model,
                            projector,
                            &plan,
                        )
                        .map(|breakdown| breakdown.total())
                    });
                Automatic {
                    blocks: shape.block_count,
                    mtp_head: memory::has_mtp_head(shape),
                    context: Some(context),
                }
            }
            None => Automatic {
                blocks: None,
                mtp_head: false,
                context: None,
            },
        };
        let (reported, metrics) = match &running {
            Some((launched, base, metrics)) => {
                let reported = self.profile_observation(base).await.ok().map_or(
                    (None, None),
                    |(props, slots)| {
                        let count = props.slots.or(u32::try_from(slots.len()).ok());
                        (
                            props
                                .per_request_tokens
                                .zip(count)
                                .map(|(tokens, count)| tokens.saturating_mul(u64::from(count))),
                            count.map(u64::from),
                        )
                    },
                );
                let metrics = if launched.mtp == Some(true) && *metrics {
                    self.engine_metrics(base)
                        .await
                        .and_then(|text| parse_mtp_metrics(&text))
                } else {
                    None
                };
                (reported, metrics)
            }
            None => ((None, None), None),
        };
        Some((
            settings_fields(
                &saved,
                running.as_ref().map(|(launched, _, _)| launched),
                reported,
                &automatic,
            ),
            metrics.unwrap_or(Value::Null),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires JUICED_TEST_LLAMA_DIR (an extracted llama.cpp release) and JUICED_TEST_GGUF"]
    async fn real_llama_fit_params_probe() {
        let root = PathBuf::from(std::env::var_os("JUICED_TEST_LLAMA_DIR").unwrap());
        let model = PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").unwrap());
        let sha256 = crate::identity::file_digest(
            &root.join(FIT_EXECUTABLE),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        let bundle = BundleSpec {
            schema_version: 1,
            implementation_id: "llama-cpp".into(),
            root: root.clone(),
            entrypoint: "llama-server".into(),
            library_directories: vec![".".into()],
            files: vec![crate::runtime_bundle::BundleFile {
                path: FIT_EXECUTABLE.into(),
                sha256,
                role: FileRole::Executable,
            }],
        };
        let context = std::env::var("JUICED_TEST_CONTEXT").unwrap_or_else(|_| "4096".into());
        let args: Vec<String> = [
            "--ctx-size",
            &context,
            "--parallel",
            "1",
            "--gpu-layers",
            "999",
            "--slots",
            "--port",
            "1",
        ]
        .map(str::to_owned)
        .into();
        let started = Instant::now();
        let devices = fit_probe(&bundle, &model, &args).await.unwrap();
        let first = started.elapsed();
        let started = Instant::now();
        assert_eq!(fit_probe(&bundle, &model, &args).await.unwrap(), devices);
        let cached = started.elapsed();
        assert!(cached < first);
        let (declared, shape) = read_shape(&model).unwrap();
        let gpu = devices
            .iter()
            .any(|device| device.device != "Host" && device.device != "CPU");
        let plan = Plan::from_arguments(&args, 4096, 1, gpu);
        let estimate = memory::llama(
            &shape,
            declared.architecture.as_deref(),
            file_bytes(&model),
            0,
            &plan,
        )
        .unwrap();
        let fitted = memory::with_fit(&estimate, &devices);
        println!(
            "devices={devices:?}\nfirst={first:?} cached={cached:?}\nestimate={estimate:?}\nfitted={fitted:?}"
        );
    }
}
