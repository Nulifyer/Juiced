//! Juiced management tools for the MCP gateway.
//!
//! Each tool replays the equivalent native route through the service router with the caller's
//! own credentials, so route authority, validation and revision conflicts stay authoritative.

use crate::auth::{Authority, RequestContext};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request, header},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

const MAX_ROUTE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MODEL_ID_BYTES: u64 = 256;

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    authority: Authority,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
    input: fn() -> Value,
}

fn model_property() -> Value {
    json!({"type":"string","minLength":1,"maxLength":MODEL_ID_BYTES,"description":"Model id from juiced_list_models."})
}
fn no_input() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
fn model_input() -> Value {
    json!({"type":"object","required":["model"],"properties":{"model":model_property()},"additionalProperties":false})
}
fn launch_property(maximum: u32, description: &str) -> Value {
    json!({"type":["integer","null"],"minimum":0,"maximum":maximum,"description":description})
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_status",
        title: "Service status",
        description: "Hardware summary, memory budget and reservations, recommended backends, and loaded models.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: no_input,
    },
    Tool {
        name: "juiced_list_models",
        title: "List models",
        description: "Every registered model with its loading policy, revision, worker state and operations. Use the revision with juiced_set_policy and juiced_set_model_settings.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: no_input,
    },
    Tool {
        name: "juiced_model_settings",
        title: "Model settings",
        description: "Saved launch settings and the memory fit estimate for one model.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: model_input,
    },
    Tool {
        name: "juiced_list_engines",
        title: "List engines",
        description: "Engines installed on this machine and the ones that can be installed here, with the generation juiced_install_engine checks.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: no_input,
    },
    Tool {
        name: "juiced_search_models",
        title: "Search models",
        description: "Search the recommended catalog, local files and Hugging Face, as the console's Find models page does.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: true,
        input: || {
            json!({"type":"object","properties":{
                "query":{"type":"string","maxLength":200,"description":"Words to match. Omit for recommendations."},
                "task":{"type":"string","enum":["chat","embeddings","transcription","tts","image"]},
                "limit":{"type":"integer","minimum":1,"maximum":20,"default":8},
                "hub":{"type":"boolean","default":true,"description":"false searches only the catalog and local files."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_list_local_files",
        title: "List local files",
        description: "Model files already on disk. Each quant's add value is the exact input for juiced_add_model.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: no_input,
    },
    Tool {
        name: "juiced_list_downloads",
        title: "List downloads",
        description: "Model downloads started from the library, with progress and outcome.",
        authority: Authority::Operator,
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: no_input,
    },
    Tool {
        name: "juiced_set_policy",
        title: "Set loading policy",
        description: "Set a model to disabled, on-demand (loads on first request) or hot (loads now and stays loaded). Fails with a conflict when expected_revision is stale.",
        authority: Authority::Operator,
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: || {
            json!({"type":"object","required":["model","policy","expected_revision"],"properties":{
                "model":model_property(),
                "policy":{"type":"string","enum":["disabled","on-demand","hot"]},
                "expected_revision":{"type":"integer","minimum":0,"description":"The model's current revision."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_load_model",
        title: "Load model",
        description: "Start a model's worker now. The model must not be disabled.",
        authority: Authority::Operator,
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: model_input,
    },
    Tool {
        name: "juiced_unload_model",
        title: "Unload model",
        description: "Stop a model's worker and free its memory. Hot models must be set to on-demand first.",
        authority: Authority::Operator,
        read_only: false,
        destructive: true,
        idempotent: true,
        open_world: false,
        input: model_input,
    },
    Tool {
        name: "juiced_set_model_settings",
        title: "Set model settings",
        description: "Change launch settings for one model. Omitted settings keep their saved value; null returns a setting to automatic. Fails with a conflict when expected_revision is stale.",
        authority: Authority::Operator,
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        input: || {
            json!({"type":"object","required":["model","expected_revision"],"properties":{
                "model":model_property(),
                "expected_revision":{"type":"integer","minimum":0},
                "context_tokens":launch_property(1_048_576, "Total context budget in tokens."),
                "gpu_layers":launch_property(1024, "Layers offloaded to the GPU."),
                "threads":launch_property(1024, "CPU threads."),
                "parallel_slots":launch_property(crate::profiles::MAX_PARALLEL_SLOTS, "Concurrent request slots."),
                "apply":{"type":"string","enum":["next_load","reload"],"default":"next_load","description":"reload restarts a loaded worker now."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_download_model",
        title: "Download model",
        description: "Start downloading a model file from Hugging Face into the library. Take repo and file from juiced_search_models.",
        authority: Authority::Operator,
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: true,
        input: || {
            json!({"type":"object","required":["repo"],"properties":{
                "repo":{"type":"string","minLength":3,"maxLength":200,"pattern":"^[^/\\s]+/[^/\\s]+$"},
                "file":{"type":"string","minLength":1,"maxLength":512},
                "files":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":512}},
                "revision":{"type":"string","minLength":1,"maxLength":128},
                "expected_generation":{"type":"integer","minimum":0,"description":"Library generation from juiced_search_models. Omit to use the current one."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_add_model",
        title: "Add model",
        description: "Register a downloaded file as a model. Pass exactly one of path, artifact or artifact_set, as listed by juiced_list_local_files. Reports engine_not_installed with the engine to install first.",
        authority: Authority::Operator,
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
        input: || {
            json!({"type":"object","properties":{
                "path":{"type":"string","minLength":1,"maxLength":4096},
                "artifact":{"type":"string","minLength":1,"maxLength":128},
                "artifact_set":{"type":"string","minLength":1,"maxLength":128},
                "id":{"type":"string","minLength":1,"maxLength":MODEL_ID_BYTES,"description":"Model id to create. Omit to derive one."},
                "policy":{"type":"string","enum":["disabled","on-demand","hot"],"default":"on-demand"},
                "backend":{"type":"string","minLength":1,"maxLength":64}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_install_engine",
        title: "Install engine",
        description: "Download and install one engine build listed as installable by juiced_list_engines.",
        authority: Authority::Operator,
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: true,
        input: || {
            json!({"type":"object","required":["engine","backend"],"properties":{
                "engine":{"type":"string","minLength":1,"maxLength":64},
                "backend":{"type":"string","minLength":1,"maxLength":64},
                "expected_generation":{"type":"integer","minimum":0,"description":"Engine generation from juiced_list_engines. Omit to use the current one."}
            },"additionalProperties":false})
        },
    },
];

fn tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

fn allows(context: &RequestContext, authority: Authority) -> bool {
    match authority {
        Authority::Operator => context.operator(),
        Authority::Inference | Authority::Read => true,
    }
}

pub(crate) fn is_management_tool(name: &str) -> bool {
    tool(name).is_some()
}

pub(crate) fn tools(context: &RequestContext) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|tool| allows(context, tool.authority))
        .map(|tool| {
            let mut annotations = json!({"title":tool.title,"readOnlyHint":tool.read_only,"openWorldHint":tool.open_world});
            if !tool.read_only {
                annotations["destructiveHint"] = json!(tool.destructive);
                annotations["idempotentHint"] = json!(tool.idempotent);
            }
            json!({"name":tool.name,"title":tool.title,"description":tool.description,"inputSchema":(tool.input)(),"annotations":annotations})
        })
        .collect()
}

#[derive(Debug)]
pub(crate) enum Failure {
    Forbidden,
    InvalidParams(String),
    Route(Value),
}

struct Routes<'a> {
    app: Router,
    headers: &'a HeaderMap,
}

impl Routes<'_> {
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, Failure> {
        let mut request = Request::builder().method(method).uri(format!("/v1/{path}"));
        for name in [
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-api-key"),
        ] {
            if let Some(value) = self.headers.get(&name) {
                request = request.header(name, value);
            }
        }
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .map_err(|_| {
                Failure::InvalidParams("The tool arguments cannot form a request.".into())
            })?;
        let unavailable = |message: &str| {
            Failure::Route(
                json!({"error":{"code":"route_unavailable","message":message,"status":503}}),
            )
        };
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .map_err(|_| unavailable("The service route did not answer."))?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), MAX_ROUTE_RESPONSE_BYTES)
            .await
            .map_err(|_| unavailable("The service response exceeded the tool limit."))?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value);
        }
        let mut error = value.get("error").cloned().filter(Value::is_object).unwrap_or_else(
            || json!({"code":"request_failed","message":format!("The service answered {status}.")}),
        );
        error["status"] = json!(status.as_u16());
        let mut failure = json!({"error":error});
        if let Some(extra) = value.as_object() {
            for (key, value) in extra.iter().filter(|(key, _)| *key != "error") {
                failure[key] = value.clone();
            }
        }
        Err(Failure::Route(failure))
    }
    async fn get(&self, path: &str) -> Result<Value, Failure> {
        self.send(Method::GET, path, None).await
    }
    async fn post(&self, path: &str, body: Value) -> Result<Value, Failure> {
        self.send(Method::POST, path, Some(body)).await
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, Failure> {
    serde_json::from_value(args).map_err(|error| {
        let message = error.to_string();
        Failure::InvalidParams(format!(
            "Tool arguments do not match the input schema: {}",
            message.chars().take(200).collect::<String>()
        ))
    })
}

fn bounded(value: &str, minimum: usize, maximum: usize, field: &str) -> Result<(), Failure> {
    if (minimum..=maximum).contains(&value.len()) {
        Ok(())
    } else {
        Err(Failure::InvalidParams(format!(
            "{field} must be {minimum} to {maximum} bytes."
        )))
    }
}

fn pick(value: &Value, keys: &[&str]) -> Value {
    let mut output = Map::new();
    for key in keys {
        if let Some(field) = value.get(*key).filter(|field| !field.is_null()) {
            output.insert((*key).to_owned(), field.clone());
        }
    }
    Value::Object(output)
}

fn list(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn present(value: &Value) -> Value {
    Value::Object(
        value
            .as_object()
            .map(|map| {
                map.iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

const MODEL_FIELDS: &[&str] = &[
    "id",
    "policy",
    "revision",
    "state",
    "loaded",
    "downloaded",
    "operations",
    "resident_bytes",
    "active_requests",
    "max_inflight",
    "error",
    "implementation_id",
    "aliased_to",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelArgs {
    model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: Option<String>,
    task: Option<crate::library::Task>,
    limit: Option<u8>,
    hub: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyArgs {
    model: String,
    policy: crate::config::Policy,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    model: String,
    expected_revision: u64,
    #[serde(default, deserialize_with = "explicit")]
    context_tokens: Option<Option<u32>>,
    #[serde(default, deserialize_with = "explicit")]
    gpu_layers: Option<Option<u32>>,
    #[serde(default, deserialize_with = "explicit")]
    threads: Option<Option<u32>>,
    #[serde(default, deserialize_with = "explicit")]
    parallel_slots: Option<Option<u32>>,
    apply: Option<String>,
}

fn explicit<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<u32>>, D::Error> {
    Option::<u32>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadArgs {
    repo: String,
    file: Option<String>,
    files: Option<Vec<String>>,
    revision: Option<String>,
    expected_generation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddArgs {
    path: Option<String>,
    artifact: Option<String>,
    artifact_set: Option<String>,
    id: Option<String>,
    policy: Option<crate::config::Policy>,
    backend: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineArgs {
    engine: String,
    backend: String,
    expected_generation: Option<u64>,
}

fn model(args: Value) -> Result<String, Failure> {
    let ModelArgs { model } = parse(args)?;
    bounded(&model, 1, MODEL_ID_BYTES as usize, "model")?;
    Ok(model)
}

fn launch(value: Option<Option<u32>>, maximum: u32, field: &str) -> Result<(), Failure> {
    match value {
        Some(Some(value)) if value > maximum => Err(Failure::InvalidParams(format!(
            "{field} must be at most {maximum}."
        ))),
        _ => Ok(()),
    }
}

pub(crate) async fn call(
    app: Router,
    headers: &HeaderMap,
    context: &RequestContext,
    name: &str,
    args: Value,
) -> Result<Value, Failure> {
    let tool = tool(name).ok_or_else(|| Failure::InvalidParams("Unknown tool.".into()))?;
    if !allows(context, tool.authority) {
        return Err(Failure::Forbidden);
    }
    let routes = Routes { app, headers };
    match tool.name {
        "juiced_status" => {
            no_arguments(args)?;
            let (state, system, catalog) = tokio::join!(
                routes.get("juiced/state"),
                routes.get("system-info"),
                routes.get("juiced/catalog")
            );
            let (state, system, catalog) = (state?, system?, catalog?);
            let resources = &state["resources"];
            let devices = &system["devices"];
            let recommended: Map<String, Value> = system["recommendations"]
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter(|(_, backend)| !backend.is_null())
                        .map(|(engine, backend)| (engine.clone(), backend.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let loaded: Vec<Value> = list(&catalog, "data")
                .iter()
                .filter(|model| model["loaded"] == true || model["state"] == "loading")
                .map(|model| {
                    pick(
                        model,
                        &["id", "state", "policy", "resident_bytes", "active_requests"],
                    )
                })
                .collect();
            Ok(json!({
                "version": state["version"],
                "paused": resources["paused"],
                "active_requests": resources["active_requests"],
                "memory": pick(resources, &["resident_budget_bytes", "reserved_bytes", "measured_memory_bytes", "reservation_source"]),
                "host": system["host"],
                "cpu": pick(&devices["cpu"], &["name", "vendor", "physical_cores", "logical_threads", "isa"]),
                "ram": devices["memory"],
                "gpus": list(devices, "gpus").iter().map(|gpu| pick(gpu, &["name", "vendor", "integrated", "vram_total_bytes", "vram_free_bytes", "gtt_total_bytes", "compute_capability", "gfx_target"])).collect::<Vec<_>>(),
                "npus": devices["npus"],
                "recommended_backends": recommended,
                "loaded_models": loaded,
                "models_registered": list(&catalog, "data").len(),
            }))
        }
        "juiced_list_models" => {
            no_arguments(args)?;
            let catalog = routes.get("juiced/catalog").await?;
            let models: Vec<Value> = list(&catalog, "data")
                .iter()
                .map(|model| pick(model, MODEL_FIELDS))
                .collect();
            Ok(json!({ "models": models }))
        }
        "juiced_model_settings" => {
            let model = encode(&model(args)?);
            let (options, fit) = (
                format!("models/{model}/options"),
                format!("models/{model}/fit"),
            );
            let (options, fit) = tokio::join!(routes.get(&options), routes.get(&fit));
            let fit = fit?;
            Ok(json!({
                "settings": options?,
                "fit": pick(&fit, &["fits", "budget_bytes", "available_bytes", "total_bytes", "reservation_bytes", "largest_context_that_fits", "trained_context", "source", "loaded", "warnings", "breakdown"]),
            }))
        }
        "juiced_list_engines" => {
            no_arguments(args)?;
            let engines = routes.get("juiced/engines").await?;
            let installable: Vec<Value> = list(&engines, "installable")
                .iter()
                .map(|row| {
                    let mut entry =
                        pick(row, &["engine", "backend", "availability", "recommended"]);
                    if let Some(bytes) = row["build"].get("archive_bytes") {
                        entry["archive_bytes"] = bytes.clone();
                    }
                    let reasons: Vec<Value> = list(row, "reasons")
                        .iter()
                        .map(|reason| reason.get("message").cloned().unwrap_or(reason.clone()))
                        .collect();
                    if !reasons.is_empty() {
                        entry["reasons"] = json!(reasons);
                    }
                    if row["installed_package"].is_string() {
                        entry["installed"] = json!(true);
                    }
                    entry
                })
                .collect();
            let installed: Vec<Value> = list(&engines, "installed")
                .iter()
                .map(|row| pick(row, &["engine", "package", "phase", "models", "release"]))
                .collect();
            Ok(json!({
                "generation": engines["generation"],
                "installation_active": engines["installation_active"],
                "recommended": present(&engines["recommendations"]),
                "installed": installed,
                "installable": installable,
                "jobs": engines["jobs"],
            }))
        }
        "juiced_search_models" => {
            let search: SearchArgs = parse(args)?;
            if search.limit.is_some_and(|limit| !(1..=20).contains(&limit)) {
                return Err(Failure::InvalidParams("limit must be 1 to 20.".into()));
            }
            let mut query = vec![format!("limit={}", search.limit.unwrap_or(8))];
            if let Some(text) = search
                .query
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                bounded(text, 1, 200, "query")?;
                query.push(format!("q={}", encode(text)));
            }
            if let Some(task) = search.task {
                query.push(format!(
                    "task={}",
                    serde_json::to_value(task)
                        .unwrap()
                        .as_str()
                        .unwrap_or("chat")
                ));
            }
            if search.hub == Some(false) {
                query.push("hub=false".into());
            }
            let result = routes
                .get(&format!("juiced/library/search?{}", query.join("&")))
                .await?;
            let models: Vec<Value> = list(&result, "data")
                .iter()
                .map(|entry| {
                    let mut model = pick(
                        entry,
                        &[
                            "repo",
                            "name",
                            "publisher",
                            "task",
                            "source",
                            "license",
                            "downloads",
                            "gated",
                            "recommended_quant",
                            "recommended_file",
                            "revision",
                        ],
                    );
                    model["quants"] = json!(
                        list(entry, "quants")
                            .iter()
                            .take(12)
                            .map(|quant| {
                                let mut row = pick(
                                    quant,
                                    &[
                                        "label",
                                        "file",
                                        "files",
                                        "bytes",
                                        "complete",
                                        "pullable",
                                        "recommended",
                                    ],
                                );
                                if quant["local"]["present"] == true {
                                    row["downloaded"] = json!(true);
                                }
                                row
                            })
                            .collect::<Vec<_>>()
                    );
                    model
                })
                .collect();
            Ok(json!({
                "generation": result["artifact_generation"],
                "offline": result["offline"],
                "next_cursor": result["page"]["next_cursor"],
                "models": models,
            }))
        }
        "juiced_list_local_files" => {
            no_arguments(args)?;
            let local = routes.get("juiced/library/local").await?;
            let groups: Vec<Value> = list(&local, "groups")
                .iter()
                .map(|group| {
                    let mut entry = pick(
                        group,
                        &["name", "repo", "publisher", "bytes", "used_by", "read_only"],
                    );
                    entry["quants"] = json!(
                        list(group, "quants")
                            .iter()
                            .map(|quant| pick(
                                quant,
                                &["label", "bytes", "complete", "parts", "used_by", "add"]
                            ))
                            .collect::<Vec<_>>()
                    );
                    entry
                })
                .collect();
            Ok(
                json!({"total_bytes": local["total_bytes"], "truncated": local["truncated"], "groups": groups}),
            )
        }
        "juiced_list_downloads" => {
            no_arguments(args)?;
            let pulls = routes.get("juiced/library/pulls").await?;
            Ok(json!({"downloads": pulls["data"]}))
        }
        "juiced_set_policy" => {
            let change: PolicyArgs = parse(args)?;
            bounded(&change.model, 1, MODEL_ID_BYTES as usize, "model")?;
            let model = routes
                .post(
                    "juiced/policy",
                    json!({"model":change.model,"policy":change.policy,"expected_revision":change.expected_revision}),
                )
                .await?;
            Ok(json!({"model": pick(&model, MODEL_FIELDS)}))
        }
        "juiced_load_model" => {
            let model = model(args)?;
            routes.post("load", json!({"model":model})).await
        }
        "juiced_unload_model" => {
            let model = model(args)?;
            routes.post("unload", json!({"model":model})).await
        }
        "juiced_set_model_settings" => {
            let change: SettingsArgs = parse(args)?;
            bounded(&change.model, 1, MODEL_ID_BYTES as usize, "model")?;
            launch(change.context_tokens, 1_048_576, "context_tokens")?;
            launch(change.gpu_layers, 1024, "gpu_layers")?;
            launch(change.threads, 1024, "threads")?;
            launch(
                change.parallel_slots,
                crate::profiles::MAX_PARALLEL_SLOTS,
                "parallel_slots",
            )?;
            let apply = change.apply.as_deref().unwrap_or("next_load");
            if !matches!(apply, "next_load" | "reload") {
                return Err(Failure::InvalidParams(
                    "apply must be next_load or reload.".into(),
                ));
            }
            let id = encode(&change.model);
            let current = routes.get(&format!("models/{id}/options")).await?;
            let saved = &current["saved"];
            let merge = |field: Option<Option<u32>>, key: &str| match field {
                Some(value) => json!(value),
                None => saved.get(key).cloned().unwrap_or(Value::Null),
            };
            let options = json!({
                "context_tokens": merge(change.context_tokens, "context_tokens"),
                "gpu_layers": merge(change.gpu_layers, "gpu_layers"),
                "threads": merge(change.threads, "threads"),
                "parallel_slots": merge(change.parallel_slots, "parallel_slots"),
            });
            routes
                .post(
                    &format!("models/{id}/options"),
                    json!({"expected_revision":change.expected_revision,"options":options,"apply":apply}),
                )
                .await
        }
        "juiced_download_model" => {
            let pull: DownloadArgs = parse(args)?;
            bounded(&pull.repo, 3, 200, "repo")?;
            for file in pull.file.iter().chain(pull.files.iter().flatten()) {
                bounded(file, 1, 512, "file")?;
            }
            if pull.files.as_ref().is_some_and(|files| files.len() > 64) {
                return Err(Failure::InvalidParams(
                    "files accepts at most 64 entries.".into(),
                ));
            }
            if let Some(revision) = &pull.revision {
                bounded(revision, 1, 128, "revision")?;
            }
            let generation = match pull.expected_generation {
                Some(generation) => generation,
                None => routes
                    .get("juiced/library/search?limit=1&hub=false")
                    .await?["artifact_generation"]
                    .as_u64()
                    .unwrap_or(0),
            };
            let mut body = json!({"repo":pull.repo,"expected_generation":generation});
            if let Some(file) = pull.file {
                body["file"] = json!(file);
            }
            if let Some(files) = pull.files {
                body["files"] = json!(files);
            }
            if let Some(revision) = pull.revision {
                body["revision"] = json!(revision);
            }
            routes.post("juiced/library/pull", body).await
        }
        "juiced_add_model" => {
            let add: AddArgs = parse(args)?;
            let sources = [&add.path, &add.artifact, &add.artifact_set]
                .iter()
                .filter(|source| source.is_some())
                .count();
            if sources != 1 {
                return Err(Failure::InvalidParams(
                    "Pass exactly one of path, artifact or artifact_set.".into(),
                ));
            }
            for (value, maximum, field) in [
                (&add.path, 4096, "path"),
                (&add.artifact, 128, "artifact"),
                (&add.artifact_set, 128, "artifact_set"),
                (&add.id, MODEL_ID_BYTES as usize, "id"),
                (&add.backend, 64, "backend"),
            ] {
                if let Some(value) = value {
                    bounded(value, 1, maximum, field)?;
                }
            }
            let mut body = Map::new();
            for (key, value) in [
                ("path", add.path),
                ("artifact", add.artifact),
                ("artifact_set", add.artifact_set),
                ("id", add.id),
                ("backend", add.backend),
            ] {
                if let Some(value) = value {
                    body.insert(key.into(), json!(value));
                }
            }
            if let Some(policy) = add.policy {
                body.insert("policy".into(), json!(policy));
            }
            routes.post("juiced/models/add", Value::Object(body)).await
        }
        "juiced_install_engine" => {
            let install: EngineArgs = parse(args)?;
            bounded(&install.engine, 1, 64, "engine")?;
            bounded(&install.backend, 1, 64, "backend")?;
            let generation = match install.expected_generation {
                Some(generation) => generation,
                None => routes.get("juiced/engines").await?["generation"]
                    .as_u64()
                    .unwrap_or(0),
            };
            routes
                .post(
                    "juiced/engines/install",
                    json!({"engine":install.engine,"backend":install.backend,"expected_generation":generation}),
                )
                .await
        }
        _ => Err(Failure::InvalidParams("Unknown tool.".into())),
    }
}

fn no_arguments(args: Value) -> Result<(), Failure> {
    match args.as_object() {
        Some(map) if map.is_empty() => Ok(()),
        _ => Err(Failure::InvalidParams(
            "This tool accepts no arguments.".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;

    fn context(role: Role) -> RequestContext {
        RequestContext {
            principal: "test",
            role,
            credential_epoch: "test".into(),
        }
    }

    #[test]
    fn management_tools_are_listed_only_to_operators_with_consistent_annotations() {
        assert!(tools(&context(Role::Inference)).is_empty());
        for role in [Role::Operator, Role::TrustedLan] {
            let listed = tools(&context(role));
            assert_eq!(listed.len(), TOOLS.len());
            for tool in &listed {
                let name = tool["name"].as_str().unwrap();
                assert!(name.starts_with("juiced_"), "{name}");
                assert_eq!(tool["inputSchema"]["type"], "object");
                assert_eq!(tool["inputSchema"]["additionalProperties"], false);
                let read_only = tool["annotations"]["readOnlyHint"].as_bool().unwrap();
                assert_eq!(
                    read_only,
                    name.starts_with("juiced_list")
                        || matches!(
                            name,
                            "juiced_status" | "juiced_model_settings" | "juiced_search_models"
                        )
                );
                assert_eq!(
                    tool["annotations"].get("destructiveHint").is_none(),
                    read_only
                );
            }
        }
    }

    #[test]
    fn path_segments_and_queries_are_percent_encoded() {
        assert_eq!(encode("fixture.chat-q4"), "fixture.chat-q4");
        assert_eq!(encode("a/b c?&"), "a%2Fb%20c%3F%26");
    }

    #[tokio::test]
    async fn inference_callers_are_refused_before_any_route_runs() {
        let refused = call(
            Router::new(),
            &HeaderMap::new(),
            &context(Role::Inference),
            "juiced_set_policy",
            json!({"model":"m","policy":"hot","expected_revision":0}),
        )
        .await;
        assert!(matches!(refused, Err(Failure::Forbidden)));
        let invalid = call(
            Router::new(),
            &HeaderMap::new(),
            &context(Role::Operator),
            "juiced_set_policy",
            json!({"model":"m","policy":"always","expected_revision":0}),
        )
        .await;
        assert!(matches!(invalid, Err(Failure::InvalidParams(_))));
        let mixed = call(
            Router::new(),
            &HeaderMap::new(),
            &context(Role::Operator),
            "juiced_add_model",
            json!({"path":"/a.gguf","artifact":"b"}),
        )
        .await;
        assert!(matches!(mixed, Err(Failure::InvalidParams(_))));
    }
}
