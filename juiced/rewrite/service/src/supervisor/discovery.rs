use super::{Manager, ModelView, Worker};
use crate::{
    config::{Operation, Policy, Provider},
    model_discovery::{
        ContextPlan, EngineFacts, LaunchFacts, TemplateFacts, context_plan, engine_facts,
        template_facts,
    },
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};

const PROPS_LIMIT: usize = 1024 * 1024;
const FILE_CACHE_ENTRIES: usize = 512;

#[derive(Clone, Debug, Default)]
struct FileFacts {
    architecture: Option<String>,
    trained: Option<u64>,
    template: Option<TemplateFacts>,
}

type EngineEntry = (u64, u32, Arc<EngineFacts>);
type FileKey = (PathBuf, u64, Option<SystemTime>);

fn file_cache() -> &'static Mutex<HashMap<FileKey, Arc<FileFacts>>> {
    static CACHE: OnceLock<Mutex<HashMap<FileKey, Arc<FileFacts>>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// Keyed by model, worker generation and pid; a worker's engine facts never change while it runs.
fn engine_cache() -> &'static Mutex<HashMap<String, EngineEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, EngineEntry>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn read_file_facts(path: &Path) -> Option<Arc<FileFacts>> {
    let metadata = std::fs::metadata(path).ok()?;
    let key = (path.to_owned(), metadata.len(), metadata.modified().ok());
    if let Some(facts) = file_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
    {
        return Some(facts.clone());
    }
    let mut file = std::fs::File::open(path).ok()?;
    let summary = crate::profiles::read_gguf_summary(&mut file).ok()?;
    let facts = Arc::new(FileFacts {
        architecture: summary.architecture,
        trained: summary.training_context_tokens,
        template: summary
            .chat_template
            .filter(|template| !template.is_empty())
            .map(|template| template_facts(&template)),
    });
    let mut cache = file_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache.len() >= FILE_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(key, facts.clone());
    Some(facts)
}

struct RunningFacts {
    base: String,
    pid: u32,
    generation: u64,
    args: Vec<String>,
}

pub(crate) struct Discovered {
    pub view: ModelView,
    pub fields: Map<String, Value>,
    pub device: &'static str,
}

impl Discovered {
    /// The catalog entry: the supervisor view plus the native-compatible discovery fields.
    pub fn entry(&self) -> Value {
        let mut entry = serde_json::to_value(&self.view).unwrap_or_else(|_| json!({}));
        if let Some(object) = entry.as_object_mut() {
            object.extend(self.fields.clone());
        }
        entry
    }

    /// An OpenAI model-list entry with the native discovery fields.
    pub fn openai(&self, id: &str, alias_of: Option<&str>) -> Value {
        let mut entry = json!({"id": id, "object": "model", "created": 0,
            "owned_by": if alias_of.is_some() { "juiced-alias" } else { "juiced" },
            "downloaded": self.view.downloaded, "loaded": self.view.loaded,
            "policy": self.view.policy, "operations": self.view.operations, "vision": self.view.vision});
        if let Some(target) = alias_of {
            entry["aliased_to"] = json!(target);
        }
        if let Some(object) = entry.as_object_mut() {
            object.extend(self.fields.clone());
        }
        entry
    }

    pub fn chat(&self) -> bool {
        self.view.operations.contains(&Operation::Chat)
    }

    /// The fields whose change matters to a client caching the model list.
    fn fingerprint(&self) -> Value {
        json!([
            self.view.id,
            self.view.policy,
            self.view.revision,
            self.view.downloaded,
            self.view.loaded,
            self.view.state,
            self.view.operations,
            self.view.vision,
            self.view.max_inflight,
            self.fields.get("context"),
            self.fields.get("thinking"),
            self.fields.get("tool_calls"),
            self.fields.get("max_output_tokens")
        ])
    }
}

pub(crate) fn revision(models: &[Discovered], aliases: &[(String, String)]) -> String {
    let mut hash = Sha256::new();
    for model in models {
        hash.update(model.fingerprint().to_string().as_bytes());
        hash.update(b"\n");
    }
    for (alias, target) in aliases {
        hash.update(format!("alias {alias} {target}\n").as_bytes());
    }
    let digest = hash.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn recipe(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::ExternalHttp => None,
        Provider::LlamaCpp => Some("llamacpp"),
        Provider::WhisperCpp => Some("whispercpp"),
        Provider::StableDiffusionCpp => Some("sd-cpp"),
        Provider::Kokoros => Some("kokoro"),
        Provider::Parakeet => Some("parakeet"),
        Provider::Moonshine => Some("moonshine"),
        Provider::Asr4all => Some("asr4all"),
        Provider::FastFlowLm => Some("flm"),
    }
}

/// Native `ModelType` names, used by `/health` and the `service` filter of `/v1/models`.
pub(crate) fn model_type(operations: &[Operation]) -> &'static str {
    if operations.iter().any(|operation| {
        matches!(
            operation,
            Operation::Chat | Operation::Completion | Operation::Responses
        )
    }) {
        "llm"
    } else if operations.contains(&Operation::Embeddings) {
        "embedding"
    } else if operations.contains(&Operation::Transcription) {
        "transcription"
    } else if operations.contains(&Operation::Speech) {
        "tts"
    } else if operations.contains(&Operation::ImageGeneration) {
        "image"
    } else {
        "unknown"
    }
}

fn device(worker: &Worker) -> &'static str {
    let profile = &worker.spec.execution_profile;
    if profile.adapter == Provider::FastFlowLm {
        return "npu";
    }
    match super::fit::bundle_gpu(profile.runtime_bundle()) {
        Some(true) => "gpu",
        Some(false) => "cpu",
        None => "unknown",
    }
}

fn text_model(operations: &[Operation]) -> bool {
    matches!(model_type(operations), "llm" | "embedding")
}

fn support(supported: Option<bool>, source: Option<&str>, reason: Option<&str>) -> Value {
    json!({"supported": supported, "source": source, "reason": reason})
}

impl Manager {
    async fn engine_props(&self, base: &str) -> Option<Value> {
        let mut response = self
            .client
            .get(format!("{base}/props"))
            .timeout(Duration::from_millis(500))
            .send()
            .await
            .ok()
            .filter(|response| response.status().is_success())?;
        let mut bytes = Vec::with_capacity(16 * 1024);
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > PROPS_LIMIT {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).ok()
    }

    async fn engine_facts_for(&self, id: &str, running: &RunningFacts) -> Option<Arc<EngineFacts>> {
        if let Some((generation, pid, facts)) = engine_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(id)
            && *generation == running.generation
            && *pid == running.pid
        {
            return Some(facts.clone());
        }
        let facts = Arc::new(engine_facts(&self.engine_props(&running.base).await?));
        if facts.per_request.is_some() {
            engine_cache()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(
                    id.to_owned(),
                    (running.generation, running.pid, facts.clone()),
                );
        }
        Some(facts)
    }

    fn running_facts(worker: &Worker) -> Option<RunningFacts> {
        let state = worker.state.try_lock().ok()?;
        let run = state.running.as_ref()?;
        Some(RunningFacts {
            base: run.base.clone(),
            pid: run.child.id()?,
            generation: run.child.generation(),
            args: run.plan.arguments().to_vec(),
        })
    }

    /// Discovery facts for every registered model. `probe` allows reading GGUF headers and
    /// asking loaded engines; without it only cached facts are used.
    pub(crate) async fn discovered(&self, probe: bool) -> Vec<Discovered> {
        let views = self.catalog().await;
        let tasks = views.into_iter().map(|view| async move {
            let worker = self.worker(&view.id).ok();
            let (fields, device) = match worker {
                Some(worker) => (
                    self.discovery_fields(&view, &worker, probe).await,
                    device(&worker),
                ),
                None => (Map::new(), "unknown"),
            };
            Discovered {
                view,
                fields,
                device,
            }
        });
        futures_util::future::join_all(tasks).await
    }

    pub(crate) async fn discovered_model(&self, id: &str) -> Option<Discovered> {
        let view = self
            .catalog()
            .await
            .into_iter()
            .find(|view| view.id == id)?;
        let worker = self.worker(id).ok()?;
        let fields = self.discovery_fields(&view, &worker, true).await;
        Some(Discovered {
            view,
            fields,
            device: device(&worker),
        })
    }

    async fn discovery_fields(
        &self,
        view: &ModelView,
        worker: &Arc<Worker>,
        probe: bool,
    ) -> Map<String, Value> {
        let profile = &worker.spec.execution_profile;
        let llama = profile.adapter == Provider::LlamaCpp;
        let configured_args = profile
            .resolved_arguments()
            .ok()
            .map(|(template, _)| template.arguments().to_vec())
            .unwrap_or_default();
        let running = view.loaded.then(|| Self::running_facts(worker)).flatten();
        let file = if llama && view.downloaded {
            let path = worker.spec.model_file.clone();
            let cached = {
                let metadata = std::fs::metadata(&path).ok();
                metadata.and_then(|metadata| {
                    file_cache()
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .get(&(path.clone(), metadata.len(), metadata.modified().ok()))
                        .cloned()
                })
            };
            match cached {
                Some(facts) => Some(facts),
                None if probe => tokio::task::spawn_blocking(move || read_file_facts(&path))
                    .await
                    .ok()
                    .flatten(),
                None => None,
            }
        } else {
            None
        };
        let engine = match &running {
            Some(running) if llama && probe => self.engine_facts_for(&view.id, running).await,
            Some(running) if llama => engine_cache()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&view.id)
                .filter(|(generation, pid, _)| {
                    *generation == running.generation && *pid == running.pid
                })
                .map(|(_, _, facts)| facts.clone()),
            _ => None,
        };
        let trained = file.as_ref().and_then(|facts| facts.trained);
        let configured = LaunchFacts::read(&configured_args);
        let applied = running
            .as_ref()
            .map(|running| LaunchFacts::read(&running.args));
        let text = text_model(&view.operations);
        let chat = view.operations.contains(&Operation::Chat);
        let mut fields = Map::new();
        fields.insert(
            "discovery_schema_version".into(),
            json!(crate::model_discovery::SCHEMA_VERSION),
        );
        fields.insert("recipe".into(), json!(recipe(profile.adapter)));
        let context = context_view(
            llama && text,
            trained,
            &configured,
            applied.as_ref(),
            engine.as_deref(),
            running.as_ref(),
        );
        let per_request = context["per_request_tokens"].clone();
        if !per_request.is_null() {
            fields.insert("context_length".into(), per_request.clone());
        }
        if let Some(trained) = trained.filter(|_| text) {
            fields.insert("max_context_window".into(), json!(trained));
        }
        let launch = applied.as_ref().unwrap_or(&configured);
        if let Some(predict) = launch.predict.filter(|_| llama && chat) {
            fields.insert("max_output_tokens".into(), json!(predict));
            fields.insert("max_completion_tokens".into(), json!(predict));
        }
        let (thinking, tools) = if !chat {
            (
                support(Some(false), Some("operations"), None),
                support(Some(false), Some("operations"), None),
            )
        } else if !llama {
            (
                support(None, None, Some("provider_unobserved")),
                support(None, None, Some("provider_unobserved")),
            )
        } else {
            chat_support(file.as_deref(), launch, engine.as_deref(), view.downloaded)
        };
        let mut labels = Vec::new();
        for (operation, label) in [
            (Operation::Chat, "chat"),
            (Operation::Embeddings, "embeddings"),
            (Operation::Transcription, "transcription"),
            (Operation::Speech, "tts"),
            (Operation::ImageGeneration, "image"),
        ] {
            if view.operations.contains(&operation) {
                labels.push(label);
            }
        }
        if view.vision {
            labels.push("vision");
        }
        if thinking["supported"] == true {
            labels.push("reasoning");
        }
        if tools["supported"] == true {
            labels.push("tool-calling");
        }
        let declared = |value: &Value| {
            if value["supported"] == true {
                json!("declared")
            } else {
                Value::Null
            }
        };
        let operations: Vec<Value> = view
            .operations
            .iter()
            .map(|operation| json!({"id": operation.path().replace('/', "."), "support": "declared", "source": "juiced_registration", "streaming": null, "conformance": "not_recorded"}))
            .collect();
        fields.insert(
            "capability_contract".into(),
            json!({
                "schema_version": 1, "model": view.id,
                "artifact": {"architecture": file.as_ref().and_then(|facts| facts.architecture.clone()), "downloaded": view.downloaded},
                "execution": {"recipe": recipe(profile.adapter), "worker_loaded": view.loaded, "settings_pending": context["settings_pending"]},
                "runtime_service": true, "operations": operations,
                "limits": {"context_tokens": context["advertised_tokens"], "context_scope": context["advertised_source"],
                    "concurrent_requests": view.max_inflight, "observed_slots": context["observed_slots"],
                    "per_request_context_tokens": per_request},
                "context": context,
                "features": {"tools": declared(&tools), "vision": if view.vision { json!("declared") } else { Value::Null }, "reasoning": declared(&thinking)},
            }),
        );
        fields.insert("labels".into(), json!(labels));
        fields.insert("context".into(), context);
        fields.insert("thinking".into(), thinking);
        fields.insert("tool_calls".into(), tools);
        fields
    }
}

fn context_view(
    text: bool,
    trained: Option<u64>,
    configured: &LaunchFacts,
    applied: Option<&LaunchFacts>,
    engine: Option<&EngineFacts>,
    running: Option<&RunningFacts>,
) -> Value {
    if !text {
        return json!({"schema_version": 1, "declared_tokens": null, "declared_source": null,
            "configured_tokens": null, "configured_automatic": false, "configured_slots": null, "kv_unified": null,
            "applied_tokens": null, "observed_per_request_tokens": null, "observed_slots": null,
            "observed_source": null, "worker_generation": null, "settings_pending": false,
            "advertised_tokens": null, "advertised_source": "unknown",
            "per_request_tokens": null, "per_request_source": null});
    }
    let configured_plan = context_plan(configured, trained);
    let applied_plan = applied.map(|applied| context_plan(applied, trained));
    let observed = engine.and_then(|engine| engine.per_request);
    let (per_request, per_request_source, advertised_source) = match (observed, &applied_plan) {
        (Some(tokens), _) => (Some(tokens), Some("observed"), "runtime_observation"),
        (
            None,
            Some(ContextPlan {
                per_request: Some(tokens),
                automatic,
                ..
            }),
        ) => (
            Some(*tokens),
            Some(if *automatic { "declared" } else { "applied" }),
            if *automatic {
                "model_metadata"
            } else {
                "applied_options"
            },
        ),
        _ => match configured_plan.per_request {
            Some(tokens) if configured_plan.automatic => {
                (Some(tokens), Some("declared"), "model_metadata")
            }
            Some(tokens) => (Some(tokens), Some("configured"), "configured_options"),
            None => (None, None, "unknown"),
        },
    };
    let plan = applied_plan.as_ref().unwrap_or(&configured_plan);
    json!({
        "schema_version": 1,
        "declared_tokens": trained, "declared_source": trained.map(|_| "model_metadata"),
        "configured_tokens": configured.context, "configured_automatic": configured_plan.automatic,
        "configured_slots": configured_plan.slots, "kv_unified": plan.unified,
        "applied_tokens": applied.and_then(|applied| applied.context),
        "observed_per_request_tokens": observed,
        "observed_slots": engine.and_then(|engine| engine.slots),
        "observed_source": observed.map(|_| "llamacpp:/props"),
        "worker_generation": running.map(|running| running.generation),
        "settings_pending": applied.is_some_and(|applied| applied.context != configured.context || applied.slots != configured.slots || applied.kv_unified != configured.kv_unified),
        "advertised_tokens": per_request, "advertised_source": advertised_source,
        "per_request_tokens": per_request, "per_request_source": per_request_source,
    })
}

fn chat_support(
    file: Option<&FileFacts>,
    launch: &LaunchFacts,
    engine: Option<&EngineFacts>,
    downloaded: bool,
) -> (Value, Value) {
    let control = |facts: &TemplateFacts| facts.thinking_control;
    let thinking_view = |facts: &TemplateFacts, source: &str| {
        let mut value = support(Some(facts.thinking), Some(source), None);
        value["request_control"] = json!(facts.thinking.then(|| control(facts)).flatten());
        value["default"] = json!(facts.thinking.then(|| launch.thinking.unwrap_or("on")));
        value
    };
    if !launch.jinja {
        let reason = Some("jinja_disabled");
        return (
            support(Some(false), Some("launch_arguments"), reason),
            support(Some(false), Some("launch_arguments"), reason),
        );
    }
    if let Some(engine) = engine
        && let Some(template) = &engine.template
    {
        let tools = support(
            engine.tools,
            Some(if engine.tools.is_some() {
                "engine_template_caps"
            } else {
                "engine_chat_template"
            }),
            None,
        );
        return (thinking_view(template, "engine_chat_template"), tools);
    }
    let template = if launch.template_override {
        match launch.inline_template.as_deref() {
            Some(inline) => Some((template_facts(inline), "launch_chat_template")),
            None => {
                let reason = Some("chat_template_overridden");
                return (support(None, None, reason), support(None, None, reason));
            }
        }
    } else {
        file.and_then(|facts| facts.template.clone())
            .map(|facts| (facts, "gguf_chat_template"))
    };
    match template {
        Some((facts, source)) => {
            let tools = support(Some(facts.tools), Some(source), None);
            (thinking_view(&facts, source), tools)
        }
        None => {
            let reason = Some(match file {
                Some(_) => "no_chat_template",
                None if downloaded => "metadata_unreadable",
                None => "weights_missing",
            });
            (support(None, None, reason), support(None, None, reason))
        }
    }
}

pub(crate) fn loaded_entry(model: &Discovered) -> Value {
    let view = &model.view;
    let context = model.fields.get("context");
    json!({
        "model_name": view.id, "type": model_type(&view.operations),
        "recipe": model.fields.get("recipe"),
        "recipe_options": {"ctx_size": context.and_then(|context| context.get("applied_tokens").cloned())},
        "device": model.device,
        "status": if view.active_requests > 0 { "busy" } else { "ready" },
        "loaded": true, "backend_alive": true, "cleanup_pending": false,
        "is_busy": view.active_requests > 0, "pinned": view.policy == Policy::Hot,
        "pid": view.pid, "worker_generation": context.and_then(|context| context.get("worker_generation").cloned()),
        "last_use": view.residency.as_ref().and_then(|residency| residency.last_used_unix_ms),
        "max_context_window": model.fields.get("max_context_window"),
        "context_length": model.fields.get("context_length"),
        "context_observation": context.map(|context| json!({"per_request_tokens": context["observed_per_request_tokens"], "slots": context["observed_slots"], "source": context["observed_source"]})),
    })
}
