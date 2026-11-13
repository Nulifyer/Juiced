use super::*;

const STORAGE_PAGE: usize = 12;
const STORAGE_KINDS: [&str; 4] = ["file", "partial", "engine", "legacy"];

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_status",
        group: Group::Inspect,
        title: "Service status",
        description: "Version, hardware summary, memory budget and headroom, loaded models, active requests, alerts that need attention, and running jobs.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: no_input,
    },
    Tool {
        name: "juiced_hardware",
        group: Group::Inspect,
        title: "Hardware",
        description: "CPU, RAM, GPUs with memory, NPUs, driver runtimes, which backends this host can run with the reason for each it cannot, and per engine the backends with a build for this host.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: no_input,
    },
    Tool {
        name: "juiced_list_models",
        group: Group::Inspect,
        title: "List models",
        description: "Registered models with policy, state, revision, task, context and reservation. Filter by task, policy, state or words.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "task":{"type":"string","enum":["chat","embeddings","transcription","tts","image","reranking","classify","audio_generation","3d"]},
                "policy":{"type":"string","enum":["disabled","on-demand","hot"]},
                "loaded":{"type":"boolean"},
                "query":{"type":"string","maxLength":100,"description":"Words in the model id."},
                "limit":{"type":"integer","minimum":1,"maximum":50,"default":25},
                "cursor":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_model_details",
        group: Group::Inspect,
        title: "Model details",
        description: "One model: files and sizes, engine, saved and effective settings, context, capabilities, memory fit at current settings, recent request speed and errors.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: model_input,
    },
    Tool {
        name: "juiced_list_engines",
        group: Group::Inspect,
        title: "List engines",
        description: "Installed engine builds and their models; installable builds with what each runs, availability here and why not, download and disk size, and the recommended backend per engine. Filter by engine.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "engine":{"type":"string","minLength":1,"maxLength":64,"description":"One engine, such as llama.cpp, vllm or acestep.cpp."},
                "available_only":{"type":"boolean","default":false,"description":"Hide builds this host cannot run."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_requests",
        group: Group::Inspect,
        title: "Recent requests",
        description: "Recent request records, newest first: timing split, queue wait, prompt processed versus cached tokens, session cache path, speed and outcome. No prompt content.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "model":model_property(),
                "outcome":{"type":"string","maxLength":32,"description":"For example completed, failed or cancelled."},
                "limit":{"type":"integer","minimum":1,"maximum":50,"default":10}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_logs",
        group: Group::Inspect,
        title: "Logs",
        description: "Bounded log lines from the service or one model. source is required; filter by level and words.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["source"],"properties":{
                "source":{"type":"string","minLength":1,"maxLength":256,"description":"service, or a model id."},
                "level":{"type":"string","enum":["error","warn","info"],"description":"This level and more severe."},
                "contains":{"type":"string","maxLength":100},
                "limit":{"type":"integer","minimum":1,"maximum":200,"default":40}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_storage",
        group: Group::Inspect,
        title: "Storage",
        description: "Free space per volume and warnings; without summary also usage by category, the storage revision, models on disk with what removing each frees, and reclaimable items with ids for juiced_delete_files.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "summary":{"type":"boolean","default":false,"description":"true returns only volumes and warnings, which is cheap."},
                "kind":{"type":"string","enum":["file","partial","engine","legacy"],"description":"List only this kind of reclaimable item."},
                "include_refused":{"type":"boolean","default":false,"description":"Also list items Juiced refuses to delete, with the reason."},
                "cursor":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_cache",
        group: Group::Inspect,
        title: "Cache",
        description: "Session cache settings, each model's effective mode and reason, saved sessions and budgets, and real reuse evidence from recent requests.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || json!({"type":"object","properties":{"model":model_property()},"additionalProperties":false}),
    },
    Tool {
        name: "juiced_connections",
        group: Group::Inspect,
        title: "Connections",
        description: "Base URL, protocols (OpenAI, Ollama, Anthropic, A1111, MCP, Realtime, Wyoming) with enabled state and health, model ids, aliases and API key counts.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: no_input,
    },
];

pub(super) fn task_of(model: &Value) -> &'static str {
    let labels = list(model, "labels");
    let has = |label: &str| labels.iter().any(|value| value == label);
    let operations = list(model, "operations");
    let serves = |operation: &str| operations.iter().any(|value| value == operation);
    if has("transcription") || serves("audio/transcriptions") {
        "transcription"
    } else if has("tts") || serves("audio/speech") {
        "tts"
    } else if has("image") || serves("images/generations") {
        "image"
    } else if has("reranking") || serves("rerank") {
        "reranking"
    } else if serves("classify") {
        "classify"
    } else if serves("audio/generations") {
        "audio_generation"
    } else if serves("3d/generations") {
        "3d"
    } else if has("chat")
        || serves("chat/completions")
        || !(has("embeddings") || serves("embeddings"))
    {
        "chat"
    } else {
        "embeddings"
    }
}

pub(super) fn model_row(model: &Value) -> Value {
    let mut row = pick(
        model,
        &[
            "id",
            "policy",
            "revision",
            "state",
            "loaded",
            "downloaded",
            "resident_bytes",
            "active_requests",
            "error",
            "recipe",
            "aliased_to",
        ],
    );
    row["task"] = json!(task_of(model));
    let context = &model["context"];
    for (key, field) in [
        ("context_tokens", "applied_tokens"),
        ("per_request_context_tokens", "per_request_tokens"),
        ("configured_context_tokens", "configured_tokens"),
    ] {
        if let Some(value) = context.get(field).filter(|value| !value.is_null()) {
            row[key] = value.clone();
        }
    }
    if model["vision"] == true {
        row["vision"] = json!(true);
    }
    row
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFilter {
    task: Option<String>,
    policy: Option<crate::config::Policy>,
    loaded: Option<bool>,
    query: Option<String>,
    limit: Option<u32>,
    cursor: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFilter {
    model: Option<String>,
    outcome: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogFilter {
    source: String,
    level: Option<String>,
    contains: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFilter {
    model: Option<String>,
}

fn free(state: &Value) -> Option<u64> {
    let resources = &state["resources"];
    Some(
        resources["resident_budget_bytes"]
            .as_u64()?
            .saturating_sub(resources["reserved_bytes"].as_u64().unwrap_or(0)),
    )
}

pub(super) fn request_row(record: &Value) -> Value {
    let mut row = pick(
        record,
        &[
            "id",
            "model",
            "alias",
            "operation",
            "outcome",
            "status",
            "elapsed_ms",
            "queue_wait_ms",
            "completed_at_ms",
            "input_tokens",
            "output_tokens",
            "prompt_processed_tokens",
            "cached_tokens",
            "prompt_tokens_per_second",
            "decode_tokens_per_second",
        ],
    );
    if let Some(cache) = record
        .get("session_cache")
        .filter(|cache| cache.is_object())
    {
        row["session_cache"] = pick(
            cache,
            &["path", "key", "slot", "restore_ms", "saved_previous"],
        );
    }
    row
}

pub(super) async fn run(context: &Context<'_>, name: &str, args: Value) -> Option<Result<Outcome>> {
    Some(match name {
        "juiced_status" => status(context, args).await,
        "juiced_hardware" => hardware(context, args).await,
        "juiced_list_models" => list_models(context, args).await,
        "juiced_model_details" => details(context, args).await,
        "juiced_list_engines" => engines(context, args).await,
        "juiced_requests" => requests(context, args).await,
        "juiced_logs" => logs(context, args).await,
        "juiced_storage" => storage(context, args).await,
        "juiced_cache" => cache(context, args).await,
        "juiced_connections" => connections(context, args).await,
        _ => return None,
    })
}

async fn status(context: &Context<'_>, args: Value) -> Result<Outcome> {
    no_arguments(&args)?;
    let (state, system, catalog, pulls, engines, sessions, storage) = tokio::join!(
        context.get("juiced/state"),
        context.get("system-info"),
        context.get("juiced/catalog"),
        context.get("juiced/library/pulls"),
        context.get("juiced/engines"),
        context.get("juiced/cache/sessions"),
        context.get("juiced/storage?summary=1"),
    );
    let (state, system, catalog) = (state?, system?, catalog?);
    let models = list(&catalog, "data");
    let resources = &state["resources"];
    let devices = &system["devices"];
    let loaded: Vec<Value> = models
        .iter()
        .filter(|model| model["loaded"] == true || model["state"] == "loading")
        .map(|model| {
            pick(
                model,
                &["id", "state", "policy", "resident_bytes", "active_requests"],
            )
        })
        .collect();
    let mut alerts: Vec<Value> = models
        .iter()
        .filter(|model| {
            !model["error"].is_null()
                || matches!(
                    model["state"].as_str(),
                    Some("failed" | "missing-weights" | "cleanup-pending")
                )
        })
        .filter(|model| model["policy"] != "disabled" || !model["error"].is_null())
        .map(|model| json!({"model":model["id"],"state":model["state"],"error":model["error"]}))
        .collect();
    let mut jobs = Vec::new();
    if let Ok(pulls) = &pulls {
        for pull in list(pulls, "data") {
            let entry = acquire::pull_job(&pull);
            if pull["state"] == "failed" {
                alerts.push(json!({"job":entry["id"],"state":"failed","error":pull["error"]}));
            }
            if pull["state"] != "complete" {
                jobs.push(entry);
            }
        }
    }
    if let Ok(engines) = &engines {
        for job in list(engines, "jobs") {
            let entry = acquire::engine_job(&job);
            if job["state"] == "failed" {
                alerts.push(json!({"job":entry["id"],"state":"failed","error":job["error"]}));
            } else if job["state"] != "complete" {
                jobs.push(entry);
            }
        }
    }
    let budget = resources["resident_budget_bytes"].as_u64();
    let headroom = free(&state);
    let cache = sessions
        .ok()
        .map(|sessions| json!({"mode":sessions["settings"]["mode"],"sessions":sessions["totals"]["sessions"],"bytes":sessions["totals"]["bytes"]}));
    let storage = storage.ok().map(|storage| {
        for warning in list(&storage, "warnings") {
            alerts.push(json!({"storage":warning["code"],"message":warning["message"]}));
        }
        json!({"volumes": list(&storage, "volumes").iter().map(volume_row).collect::<Vec<_>>()})
    });
    let summary = format!(
        "Juiced {}: {} of {} models loaded holding {}, {} free of {} model budget, {} active requests, {}{}.",
        state["version"].as_str().unwrap_or("unknown"),
        loaded.len(),
        models.len(),
        size(resources["reserved_bytes"].as_u64().unwrap_or(0)),
        headroom.map_or("unknown".into(), size),
        budget.map_or("unknown".into(), size),
        resources["active_requests"].as_u64().unwrap_or(0),
        if alerts.is_empty() {
            "no alerts".to_owned()
        } else {
            format!("{} alerts", alerts.len())
        },
        if resources["paused"] == true {
            ", admission paused"
        } else {
            ""
        },
    );
    let mut outcome = Outcome::new(
        summary,
        json!({
            "version": state["version"],
            "paused": resources["paused"],
            "active_requests": resources["active_requests"],
            "memory": {
                "budget_bytes": budget, "reserved_bytes": resources["reserved_bytes"],
                "free_bytes": headroom, "measured_bytes": resources["measured_memory_bytes"],
                "source": resources["reservation_source"],
                "change_budget": "The operator sets the budget with Model memory on the Loading policy screen.",
            },
            "host": system["host"],
            "cpu": pick(&devices["cpu"], &["name", "logical_threads"]),
            "ram_bytes": devices["memory"]["total_bytes"],
            "gpus": list(devices, "gpus").iter().map(|gpu| pick(gpu, &["name", "vendor", "vram_total_bytes", "vram_free_bytes"])).collect::<Vec<_>>(),
            "npus": list(devices, "npus").len(),
            "loaded_models": loaded,
            "models_registered": models.len(),
            "alerts": alerts,
            "jobs": jobs,
            "cache": cache,
            "storage": storage,
        }),
    );
    if !jobs.is_empty() {
        outcome = outcome.next(&["juiced_wait_job"]);
    }
    Ok(outcome)
}

async fn hardware(context: &Context<'_>, args: Value) -> Result<Outcome> {
    no_arguments(&args)?;
    let (system, engines) = tokio::join!(context.get("system-info"), context.get("juiced/engines"));
    let system = system?;
    let devices = &system["devices"];
    let mut usable: Vec<String> = Vec::new();
    let mut unavailable = Map::new();
    for row in list(&system, "backends") {
        let name = format!(
            "{}:{}",
            row["recipe"].as_str().unwrap_or(""),
            row["backend"].as_str().unwrap_or("")
        );
        if row["state"] == "unavailable" {
            let backend = row["backend"].as_str().unwrap_or("unknown").to_owned();
            if !unavailable.contains_key(&backend) {
                let reason = list(&row, "reasons")
                    .iter()
                    .filter_map(|reason| reason["message"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join(" ");
                unavailable.insert(backend, json!(reason));
            }
        } else if !usable.contains(&name) {
            usable.push(name);
        }
    }
    let usable_backends: Vec<&str> = usable
        .iter()
        .filter_map(|name| name.split(':').nth(1))
        .collect();
    unavailable.retain(|backend, _| !usable_backends.contains(&backend.as_str()));
    let gpus: Vec<Value> = list(devices, "gpus")
        .iter()
        .map(|gpu| {
            pick(
                gpu,
                &[
                    "name",
                    "vendor",
                    "integrated",
                    "vram_total_bytes",
                    "vram_free_bytes",
                    "gtt_total_bytes",
                    "compute_capability",
                    "gfx_target",
                    "driver",
                ],
            )
        })
        .collect();
    let summary = format!(
        "{} with {} threads, {} RAM, {} GPU(s), {} NPU(s). {} backend(s) usable here.",
        devices["cpu"]["name"].as_str().unwrap_or("Unknown CPU"),
        devices["cpu"]["logical_threads"].as_u64().unwrap_or(0),
        devices["memory"]["total_bytes"]
            .as_u64()
            .map_or("unknown".into(), size),
        gpus.len(),
        list(devices, "npus").len(),
        usable.len(),
    );
    let mut outcome = Outcome::new(
        summary,
        json!({
            "host": system["host"],
            "cpu": pick(&devices["cpu"], &["name", "vendor", "physical_cores", "logical_threads", "isa"]),
            "memory": pick(&devices["memory"], &["total_bytes", "available_bytes"]),
            "gpus": gpus,
            "npus": devices["npus"],
            "runtimes": {"cuda": devices["cuda"], "rocm": devices["rocm"], "vulkan": pick(&devices["vulkan"], &["state", "basis"])},
            "recommended": present(&system["recommendations"]),
            "usable": usable,
            "unavailable_backends": unavailable,
            "issues": system["issues"],
        }),
    );
    if let Ok(engines) = engines {
        outcome.data["engine_generation"] = engines["generation"].clone();
        let mut by_engine: Map<String, Value> = Map::new();
        for row in list(&engines, "installable") {
            let Some(engine) = row["engine"].as_str() else {
                continue;
            };
            let entry = by_engine
                .entry(engine.to_owned())
                .or_insert_with(|| json!({"uses": engine_use(engine), "here": [], "not_here": []}));
            let backend = row["backend"].clone();
            let key = if installable_here(&row) {
                "here"
            } else {
                "not_here"
            };
            if let Some(backends) = entry[key].as_array_mut()
                && !backends.contains(&backend)
            {
                backends.push(backend);
            }
        }
        let runnable = by_engine
            .values()
            .filter(|entry| {
                entry["here"]
                    .as_array()
                    .is_some_and(|here| !here.is_empty())
            })
            .count();
        outcome.summary = format!(
            "{} {} of {} engines have a build for this host.",
            outcome.summary,
            runnable,
            by_engine.len()
        );
        outcome.data["engines"] = Value::Object(by_engine);
    }
    Ok(outcome.next(&["juiced_list_engines"]))
}

async fn list_models(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let filter: ModelFilter = parse(args)?;
    let size = limit(filter.limit, 25, 50, "limit")? as usize;
    if let Some(query) = &filter.query {
        bounded(query, 0, 100, "query")?;
    }
    let catalog = context.get("juiced/catalog").await?;
    let words: Vec<String> = filter
        .query
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let all = list(&catalog, "data");
    let rows: Vec<Value> = all
        .iter()
        .filter(|model| {
            filter
                .task
                .as_deref()
                .is_none_or(|task| task_of(model) == task)
        })
        .filter(|model| {
            filter
                .policy
                .is_none_or(|policy| model["policy"] == json!(policy))
        })
        .filter(|model| filter.loaded.is_none_or(|loaded| model["loaded"] == loaded))
        .filter(|model| {
            let id = model["id"].as_str().unwrap_or("").to_lowercase();
            words.iter().all(|word| id.contains(word))
        })
        .map(model_row)
        .collect();
    let matched = rows.len();
    let page = page(rows, filter.cursor.unwrap_or(0), size);
    let loaded = all.iter().filter(|model| model["loaded"] == true).count();
    Ok(Outcome::new(
        format!("{matched} of {} models match; {loaded} loaded.", all.len()),
        json!({"catalog_revision": catalog["revision"], "models": page}),
    )
    .next(&["juiced_model_details"]))
}

async fn details(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let model = model_arg(args)?.model;
    let id = encode(&model);
    let entry = context.model(&model).await?;
    let paths = [
        format!("models/{id}/options"),
        format!("models/{id}/fit"),
        format!("models/{id}/files"),
        format!("models/{id}/runtime"),
    ];
    let (options, fit, files, records, runtime) = tokio::join!(
        context.get(&paths[0]),
        context.get(&paths[1]),
        context.get(&paths[2]),
        context.get("juiced/requests"),
        context.get(&paths[3]),
    );
    let mut warnings = Vec::new();
    let mut data = model_row(&entry);
    data["operations"] = entry["operations"].clone();
    data["capabilities"] = json!({
        "vision": entry["vision"],
        "thinking": entry["thinking"]["supported"],
        "tools": entry["tool_calls"]["supported"],
    });
    data["context"] = present(&entry["context"]);
    match options {
        Ok(options) => {
            data["settings"] = pick(
                &options,
                &[
                    "revision",
                    "provider",
                    "saved",
                    "effective_launch",
                    "physical_slots",
                    "reservation_bytes",
                    "image_defaults",
                    "context_semantics",
                ],
            );
        }
        Err(failure) => warnings.push(format!(
            "Settings unavailable: {}",
            failure.error()["message"].as_str().unwrap_or("error")
        )),
    }
    match fit {
        Ok(fit) => {
            data["fit"] = pick(
                &fit,
                &[
                    "fits",
                    "total_bytes",
                    "available_bytes",
                    "budget_bytes",
                    "largest_context_that_fits",
                    "trained_context",
                    "source",
                    "warnings",
                ],
            );
        }
        Err(failure) => {
            data["fit"] = json!({"available": false, "reason": failure.error()["message"]});
        }
    }
    if let Ok(files) = files {
        data["files"] = json!(
            list(&files, "files")
                .iter()
                .map(|file| pick(file, &["name", "role", "size_bytes", "exists"]))
                .collect::<Vec<_>>()
        );
        data["file_bytes"] = json!(
            list(&files, "files")
                .iter()
                .filter_map(|file| file["size_bytes"].as_u64())
                .sum::<u64>()
        );
    }
    if let Ok(runtime) = runtime {
        data["engine"] = pick(
            &runtime,
            &[
                "adapter",
                "current",
                "previous",
                "compatibility",
                "supported",
            ],
        );
    }
    if let Ok(records) = records {
        let recent: Vec<Value> = list(&records, "data")
            .iter()
            .rev()
            .filter(|record| record["model"] == model.as_str())
            .take(5)
            .map(request_row)
            .collect();
        data["recent_requests"] = json!(recent);
    }
    let summary = format!(
        "{} is {} ({}), {}.",
        model,
        entry["state"].as_str().unwrap_or("unknown"),
        entry["policy"].as_str().unwrap_or("unknown"),
        match data["fit"]["fits"].as_bool() {
            Some(true) => "fits the memory budget at its current settings",
            Some(false) => "does not fit the memory budget at its current settings",
            None => "no memory estimate for its engine",
        }
    );
    let mut outcome =
        Outcome::new(summary, data).next(&["juiced_set_model_settings", "juiced_fit"]);
    outcome.warnings = warnings;
    Ok(outcome)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineFilter {
    engine: Option<String>,
    available_only: Option<bool>,
}

/// What each engine runs, so the model can match a task to an engine without guessing.
pub(super) fn engine_use(engine: &str) -> &'static str {
    match engine {
        "llama.cpp" => {
            "chat and embeddings from GGUF; image input when the model has an mmproj file"
        }
        "fastflowlm" => "chat on the XDNA2 NPU from FLM checkpoints",
        "vllm" => {
            "chat from Transformers checkpoint folders (safetensors), including vision-language models"
        }
        "ds4" => "DeepSeek V4 text chat from its own GGUFs, no image input",
        "onnxruntime" => "text classification from exported ONNX encoders",
        "whisper.cpp" | "parakeet.cpp" | "moonshine" => "speech to text",
        "futo-transcribe.cpp" => "speech to text, English only",
        "kokoros" => "text to speech",
        "openmoss" => "text to speech and sound effects",
        "acestep.cpp" => "music",
        "thinksound.cpp" => "sound effects",
        "sd.cpp" => "images",
        "thenoise" => "images from split diffusion safetensors",
        "trellis.cpp" => "image to 3D mesh",
        _ => "",
    }
}

fn engine_note(engine: &str) -> Option<&'static str> {
    match engine {
        "vllm" => Some(
            "Installs as a Python environment; the first load compiles kernels and can take several minutes.",
        ),
        "thenoise" => Some("The first image after loading is slow while PyTorch compiles kernels."),
        _ => None,
    }
}

/// The engine's public name for any spelling `Engine::parse` accepts.
pub(super) fn engine_name(engine: &str) -> Option<&'static str> {
    crate::engines::catalog::Engine::parse(engine)
        .or_else(|| {
            crate::engines::catalog::Engine::all().find(|known| {
                known.name().eq_ignore_ascii_case(engine)
                    || known.recipe().eq_ignore_ascii_case(engine)
            })
        })
        .map(crate::engines::catalog::Engine::name)
}

/// A build this host could install: not ruled out by detection and resolved to a release.
pub(super) fn installable_here(row: &Value) -> bool {
    row["availability"] != "unavailable" && row["unresolved"].is_null() && row["build"].is_object()
}

fn unresolved_reason(row: &Value) -> Option<&'static str> {
    Some(match row["unresolved"].as_str()? {
        "host_unsupported" => {
            "No build is published for this operating system and CPU architecture."
        }
        "no_matching_gpu" => "No build matches a GPU detected here.",
        "no_matching_npu" => "No build matches an NPU detected here.",
        "cpu_unsupported" => "This CPU lacks instructions the build needs.",
        "not_published" => "No build is published yet.",
        _ => "No build is known for this backend.",
    })
}

pub(super) fn installable_row(row: &Value, full: bool) -> Value {
    let engine = row["engine"].as_str().unwrap_or("");
    let mut reasons: Vec<Value> = list(row, "reasons")
        .iter()
        .map(|reason| reason.get("message").cloned().unwrap_or(reason.clone()))
        .collect();
    if let Some(reason) = unresolved_reason(row) {
        reasons.push(json!(reason));
    }
    let available = installable_here(row);
    let mut entry = pick(row, &["engine", "backend", "availability"]);
    if !full && !available {
        if let Some(reason) = reasons.first() {
            entry["why_not"] = reason.clone();
        }
        return entry;
    }
    entry["uses"] = json!(engine_use(engine));
    if row["recommended"] == true {
        entry["recommended"] = json!(true);
    }
    if row["installed_package"].is_string() {
        entry["installed"] = json!(true);
    }
    let build = &row["build"];
    if let Some(bytes) = build.get("archive_bytes").filter(|bytes| bytes.is_u64()) {
        entry["download_bytes"] = bytes.clone();
    }
    if let Some(tag) = build.get("tag") {
        entry["release"] = tag.clone();
    }
    if let Some(environment) = build.get("environment").filter(|value| value.is_object()) {
        entry["kind"] = json!("environment");
        entry["disk_bytes"] = environment["installed_bytes"].clone();
        if let Some(version) = environment.get("version") {
            entry["version"] = version.clone();
        }
    } else if let Some(environment) = crate::engines::catalog::builds()
        .filter(|known| known.engine.name() == engine && row["backend"] == known.backend)
        .find_map(|known| known.environment)
    {
        entry["kind"] = json!("environment");
        entry["disk_bytes"] = json!(environment.installed_bytes);
        entry["version"] = json!(environment.version);
    }
    if let Some(note) = engine_note(engine) {
        entry["note"] = json!(note);
    }
    if !reasons.is_empty() {
        entry["why_not"] = json!(reasons);
    }
    entry
}

pub(super) async fn engines(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let filter: EngineFilter = parse(args)?;
    let wanted = match &filter.engine {
        Some(engine) => {
            bounded(engine, 1, 64, "engine")?;
            Some(engine_name(engine).ok_or_else(|| {
                Failure::new(
                    "unknown_engine",
                    format!(
                        "{engine} is not an engine. Engines: {}.",
                        crate::engines::catalog::Engine::all()
                            .map(crate::engines::catalog::Engine::name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    404,
                )
            })?)
        }
        None => None,
    };
    let engines = context.get("juiced/engines").await?;
    let matches = |row: &Value| wanted.is_none_or(|engine| row["engine"] == engine);
    let rows: Vec<Value> = list(&engines, "installable")
        .into_iter()
        .filter(|row| matches(row))
        .collect();
    let installable: Vec<Value> = rows
        .iter()
        .filter(|row| filter.available_only != Some(true) || installable_here(row))
        .map(|row| installable_row(row, wanted.is_some()))
        .collect();
    let installed: Vec<Value> = list(&engines, "installed")
        .iter()
        .filter(|row| matches(row))
        .map(|row| {
            let mut entry = pick(
                row,
                &["engine", "package", "kind", "phase", "reserved_bytes"],
            );
            if let Some(release) = row["release"].as_object() {
                entry["backend"] = release.get("backend").cloned().unwrap_or(Value::Null);
                entry["release"] = release
                    .get("tag")
                    .or(release.get("release"))
                    .cloned()
                    .unwrap_or(Value::Null);
            } else if row["release"].is_string() {
                entry["release"] = row["release"].clone();
            }
            entry["models"] = json!(
                list(row, "models")
                    .iter()
                    .filter_map(|model| model.get("model").cloned())
                    .collect::<Vec<_>>()
            );
            entry
        })
        .collect();
    let jobs: Vec<Value> = list(&engines, "jobs")
        .iter()
        .filter(|job| matches(job))
        .map(acquire::engine_job)
        .collect();
    let usable = rows
        .iter()
        .filter(|row| installable_here(row) && row["installed_package"].is_null())
        .count();
    let recommended: Map<String, Value> = engines["recommendations"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, backend)| !backend.is_null())
        .filter(|(recipe, _)| wanted.is_none_or(|engine| engine_name(recipe) == Some(engine)))
        .map(|(recipe, backend)| (recipe.clone(), backend.clone()))
        .collect();
    let mut outcome = Outcome::new(
        format!(
            "{} installed engine build(s); {} more installable here{}.",
            installed.len(),
            usable,
            match wanted {
                Some(engine) => format!(" for {engine}"),
                None => String::new(),
            }
        ),
        json!({
            "generation": engines["generation"],
            "installation_active": engines["installation_active"],
            "recommended": recommended,
            "installed": installed,
            "installable": installable,
            "jobs": jobs,
        }),
    )
    .next(&["juiced_install_engine"]);
    if wanted.is_none() {
        outcome = outcome.warn(
            "Builds this host cannot run show only the first reason; pass engine for every detail.",
        );
    }
    Ok(outcome)
}

async fn requests(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let filter: RequestFilter = parse(args)?;
    if let Some(model) = &filter.model {
        model_id(model)?;
    }
    let size = limit(filter.limit, 10, 50, "limit")? as usize;
    let records = context.get("juiced/requests").await?;
    let all = list(&records, "data");
    let rows: Vec<Value> = all
        .iter()
        .rev()
        .filter(|record| {
            filter
                .model
                .as_deref()
                .is_none_or(|model| record["model"] == model)
        })
        .filter(|record| {
            filter
                .outcome
                .as_deref()
                .is_none_or(|outcome| record["outcome"] == outcome)
        })
        .take(size)
        .map(request_row)
        .collect();
    let summary = if rows.is_empty() {
        "No matching request records. Records keep the latest 128 requests and reset on restart."
            .to_owned()
    } else {
        let slowest = rows
            .iter()
            .filter_map(|row| row["elapsed_ms"].as_u64())
            .max()
            .unwrap_or(0);
        format!(
            "{} request record(s), newest first; slowest took {slowest} ms.",
            rows.len()
        )
    };
    Ok(Outcome::new(
        summary,
        json!({"records": rows, "retained": all.len(), "capacity": records["capacity"], "contains_content": false}),
    ))
}

fn severity(level: &str) -> u8 {
    match level {
        "error" => 3,
        "warn" | "warning" => 2,
        "info" => 1,
        _ => 0,
    }
}

async fn logs(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let filter: LogFilter = parse(args)?;
    bounded(&filter.source, 1, 256, "source")?;
    if let Some(words) = &filter.contains {
        bounded(words, 0, 100, "contains")?;
    }
    let size = limit(filter.limit, 40, 200, "limit")? as usize;
    let minimum = filter.level.as_deref().map_or(0, severity);
    let contains = filter.contains.as_deref().unwrap_or("").to_lowercase();
    let page = context
        .get(&format!(
            "juiced/logs?limit={}&source={}",
            crate::logs::MAXIMUM_PAGE,
            encode(&filter.source)
        ))
        .await?;
    let lines: Vec<Value> = list(&page, "data")
        .iter()
        .filter(|line| severity(line["level"].as_str().unwrap_or("")) >= minimum)
        .filter(|line| {
            contains.is_empty()
                || line["text"]
                    .as_str()
                    .is_some_and(|text| text.to_lowercase().contains(&contains))
        })
        .map(|line| {
            let mut text = line["text"].as_str().unwrap_or("").to_owned();
            if text.len() > 400 {
                let mut end = 400;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str(" [truncated]");
            }
            json!({"seq":line["seq"],"at_ms":line["timestamp_ms"],"level":line["level"],"text":text})
        })
        .collect();
    let shown: Vec<Value> = lines[lines.len().saturating_sub(size)..].to_vec();
    Ok(Outcome::new(
        format!(
            "{} matching line(s) from {}, latest {} shown.",
            lines.len(),
            filter.source,
            shown.len()
        ),
        json!({"source": filter.source, "lines": shown, "dropped": page["dropped"]}),
    ))
}

async fn cache(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let filter: CacheFilter = parse(args)?;
    if let Some(model) = &filter.model {
        model_id(model)?;
    }
    let (sessions, snapshots, records) = tokio::join!(
        context.get("juiced/cache/sessions"),
        context.get("juiced/cache"),
        context.get("juiced/requests"),
    );
    let sessions = sessions?;
    let models: Vec<Value> = list(&sessions, "models")
        .iter()
        .filter(|row| {
            filter
                .model
                .as_deref()
                .is_none_or(|model| row["model"] == model)
        })
        .map(|row| {
            let mut entry = pick(
                row,
                &[
                    "model",
                    "mode",
                    "effective",
                    "reason",
                    "sessions",
                    "used_bytes",
                    "budget_bytes",
                ],
            );
            entry["slots"] = json!(list(row, "slots").len());
            entry
        })
        .collect();
    if let Some(model) = &filter.model
        && models.is_empty()
    {
        return Err(Failure::new(
            "unknown_model",
            format!("No session cache entry for {model}. Use juiced_list_models."),
            404,
        ));
    }
    let saved: Vec<Value> = list(&sessions, "sessions")
        .iter()
        .filter(|session| {
            filter
                .model
                .as_deref()
                .is_none_or(|model| session["model"] == model)
        })
        .take(10)
        .map(|session| {
            pick(
                session,
                &[
                    "session",
                    "model",
                    "bytes",
                    "tokens",
                    "last_used_unix_ms",
                    "execution_current",
                ],
            )
        })
        .collect();
    let mut evidence = json!({"requests": 0, "with_cached_tokens": 0, "restored": 0, "cached_tokens": 0, "processed_tokens": 0, "input_tokens": 0});
    let mut paths = Map::new();
    if let Ok(records) = &records {
        for record in list(records, "data").iter().filter(|record| {
            filter
                .model
                .as_deref()
                .is_none_or(|model| record["model"] == model)
        }) {
            evidence["requests"] = json!(evidence["requests"].as_u64().unwrap_or(0) + 1);
            let cached = record["cached_tokens"].as_u64().unwrap_or(0);
            if cached > 0 {
                evidence["with_cached_tokens"] =
                    json!(evidence["with_cached_tokens"].as_u64().unwrap_or(0) + 1);
            }
            for (key, field) in [
                ("cached_tokens", "cached_tokens"),
                ("processed_tokens", "prompt_processed_tokens"),
                ("input_tokens", "input_tokens"),
            ] {
                evidence[key] = json!(
                    evidence[key].as_u64().unwrap_or(0) + record[field].as_u64().unwrap_or(0)
                );
            }
            if let Some(path) = record["session_cache"]["path"].as_str() {
                let count = paths.get(path).and_then(Value::as_u64).unwrap_or(0) + 1;
                paths.insert(path.to_owned(), json!(count));
                if record["session_cache"]["restore_ms"].is_u64() {
                    evidence["restored"] = json!(evidence["restored"].as_u64().unwrap_or(0) + 1);
                }
            }
        }
    }
    evidence["session_paths"] = Value::Object(paths);
    let effective_on = models
        .iter()
        .filter(|row| row["effective"] != "off")
        .count();
    let reused = evidence["with_cached_tokens"].as_u64().unwrap_or(0)
        + evidence["restored"].as_u64().unwrap_or(0);
    let summary = format!(
        "Session cache mode {}; effective for {} of {} model(s); {} of {} recent request(s) show reuse.",
        sessions["settings"]["mode"].as_str().unwrap_or("unknown"),
        effective_on,
        models.len(),
        reused.min(evidence["requests"].as_u64().unwrap_or(0)),
        evidence["requests"],
    );
    let mut data = json!({
        "configuration_generation": sessions["configuration_generation"],
        "settings": sessions["settings"],
        "totals": sessions["totals"],
        "models": models,
        "saved_sessions": saved,
        "reuse_evidence": evidence,
    });
    if let Ok(snapshots) = snapshots {
        data["explicit_snapshots"] = json!({"enabled": snapshots["enabled"], "count": list(&snapshots, "data").len(), "qualification": snapshots["qualification"]});
    }
    Ok(Outcome::new(summary, data).next(&["juiced_set_cache", "juiced_forget_sessions"]))
}

async fn connections(context: &Context<'_>, args: Value) -> Result<Outcome> {
    no_arguments(&args)?;
    let (health, access, aliases, wyoming, catalog, voices) = tokio::join!(
        context.get("juiced/protocols/health"),
        context.get("juiced/access"),
        context.get("juiced/aliases"),
        context.get("juiced/wyoming"),
        context.get("juiced/catalog"),
        context.get("juiced/voices"),
    );
    let health = health?;
    let base = context.host();
    let protocols: Vec<Value> = list(&health, "protocols")
        .iter()
        .map(|row| {
            let mut entry = pick(
                row,
                &["id", "title", "enabled", "status", "support", "models"],
            );
            let path = match row["id"].as_str() {
                Some("openai") => Some("/v1"),
                Some("ollama") => Some("/api"),
                Some("anthropic") => Some("/v1/messages"),
                Some("automatic1111") => Some("/sdapi/v1"),
                Some("mcp") => Some("/mcp"),
                Some("realtime") => Some("/v1/realtime"),
                _ => None,
            };
            if let (Some(base), Some(path)) = (&base, path) {
                entry["url"] = json!(format!("{base}{path}"));
            }
            entry
        })
        .collect();
    let mut data = json!({"base_url": base, "protocols": protocols});
    if let Ok(access) = access {
        let keys = list(&access, "keys");
        let mut scopes = Map::new();
        for key in &keys {
            let scope = key["scope"].as_str().unwrap_or("unknown").to_owned();
            let count = scopes.get(&scope).and_then(Value::as_u64).unwrap_or(0) + 1;
            scopes.insert(scope, json!(count));
        }
        data["access"] = json!({"mode": access["mode"], "keys": keys.len(), "by_scope": scopes, "keys_required": access["mode"] == "keys"});
    }
    if let Ok(aliases) = aliases {
        data["aliases"] = json!(
            list(&aliases, "aliases")
                .iter()
                .map(|alias| pick(alias, &["alias", "target"]))
                .collect::<Vec<_>>()
        );
        data["alias_revision"] = aliases["revision"].clone();
    }
    if let Ok(wyoming) = wyoming {
        data["wyoming"] = json!({
            "settings": wyoming["settings"], "listening": wyoming["listener"]["listening"],
            "error": wyoming["listener"]["error"], "resolved": wyoming["resolved"],
            "configuration_generation": wyoming["configuration_generation"],
        });
    }
    if let Ok(catalog) = catalog {
        data["model_ids"] = json!(
            list(&catalog, "data")
                .iter()
                .filter(|model| model["policy"] != "disabled")
                .map(|model| json!({"id": model["id"], "task": task_of(model)}))
                .collect::<Vec<_>>()
        );
    }
    if let Ok(voices) = voices {
        data["tts_voices"] = json!(
            list(&voices, "data")
                .iter()
                .take(20)
                .map(|voice| voice.get("id").cloned().unwrap_or(voice.clone()))
                .collect::<Vec<_>>()
        );
    }
    let enabled = data["protocols"].as_array().map_or(0, |rows| {
        rows.iter().filter(|row| row["enabled"] == true).count()
    });
    Ok(Outcome::new(
        format!(
            "{} of {} protocols enabled; access mode {}.",
            enabled,
            data["protocols"].as_array().map_or(0, Vec::len),
            data["access"]["mode"].as_str().unwrap_or("unknown")
        ),
        data,
    )
    .next(&[
        "juiced_set_protocol",
        "juiced_create_api_key",
        "juiced_set_alias",
    ]))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageArgs {
    summary: Option<bool>,
    kind: Option<String>,
    include_refused: Option<bool>,
    cursor: Option<usize>,
}

pub(super) fn volume_row(volume: &Value) -> Value {
    pick(
        volume,
        &[
            "path",
            "roles",
            "total_bytes",
            "available_bytes",
            "read_only",
            "low",
        ],
    )
}

pub(super) fn storage_item(item: &Value) -> Value {
    let mut row = pick(
        item,
        &["id", "kind", "title", "bytes", "reclaimable", "message"],
    );
    if let Some(references) = item["references"]
        .as_array()
        .filter(|references| !references.is_empty())
    {
        row["used_by"] = json!(references.iter().take(6).collect::<Vec<_>>());
    }
    row
}

async fn storage(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let args: StorageArgs = parse(args)?;
    if let Some(kind) = &args.kind
        && !STORAGE_KINDS.contains(&kind.as_str())
    {
        return Err(Failure::InvalidParams(
            "kind must be file, partial, engine or legacy.".into(),
        ));
    }
    let volumes =
        |view: &Value| -> Vec<Value> { list(view, "volumes").iter().map(volume_row).collect() };
    let free_line = |rows: &[Value]| -> String {
        rows.iter()
            .filter(|row| row["read_only"] != true)
            .map(|row| {
                format!(
                    "{} free on {}",
                    row["available_bytes"].as_u64().map_or("?".into(), size),
                    row["path"].as_str().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    if args.summary == Some(true) {
        if args.kind.is_some() || args.include_refused.is_some() || args.cursor.is_some() {
            return Err(Failure::InvalidParams(
                "summary: true takes no other arguments.".into(),
            ));
        }
        let view = context.get("juiced/storage?summary=1").await?;
        let rows = volumes(&view);
        let warnings = list(&view, "warnings");
        let mut outcome = Outcome::new(
            format!(
                "{}{}.",
                free_line(&rows),
                if warnings.is_empty() {
                    String::new()
                } else {
                    format!("; {} warning(s)", warnings.len())
                }
            ),
            json!({"volumes": rows, "warnings": warnings, "settings": view["settings"]}),
        );
        outcome.next.push("juiced_storage");
        return Ok(outcome);
    }
    let view = context.get("juiced/storage").await?;
    let include_refused = args.include_refused == Some(true);
    let mut items: Vec<Value> = ["unreferenced", "partials", "engines", "legacy"]
        .iter()
        .flat_map(|key| list(&view, key))
        .filter(|item| args.kind.as_deref().is_none_or(|kind| item["kind"] == kind))
        .collect();
    let refused = items
        .iter()
        .filter(|item| item["reclaimable"] != true)
        .count();
    if !include_refused {
        items.retain(|item| item["reclaimable"] == true);
    }
    items.sort_by(|left, right| right["bytes"].as_u64().cmp(&left["bytes"].as_u64()));
    let reclaimable_bytes: u64 = items
        .iter()
        .filter(|item| item["reclaimable"] == true)
        .filter_map(|item| item["bytes"].as_u64())
        .sum();
    let reclaimable_count = items
        .iter()
        .filter(|item| item["reclaimable"] == true)
        .count();
    let rows: Vec<Value> = items.iter().map(storage_item).collect();
    let categories: Vec<Value> = list(&view, "categories")
        .iter()
        .filter(|row| row["bytes"].as_u64().unwrap_or(0) > 0)
        .map(|row| {
            pick(
                row,
                &["id", "title", "bytes", "reclaimable_bytes", "truncated"],
            )
        })
        .collect();
    let models: Vec<Value> = list(&view, "models")
        .iter()
        .take(10)
        .map(|row| {
            let mut entry = pick(
                row,
                &["model", "bytes", "removal_frees_bytes", "read_only_bytes"],
            );
            if let Some(shared) = row["shared_with"]
                .as_array()
                .filter(|shared| !shared.is_empty())
            {
                entry["shared_with"] = json!(shared);
            }
            entry
        })
        .collect();
    let volume_rows = volumes(&view);
    let warnings = list(&view, "warnings");
    let legacy_bytes: u64 = list(&view, "legacy")
        .iter()
        .filter(|item| item["reclaimable"] == true)
        .filter_map(|item| item["bytes"].as_u64())
        .sum();
    let summary = format!(
        "{}. {} reclaimable in {} item(s){}.",
        free_line(&volume_rows),
        size(reclaimable_bytes),
        reclaimable_count,
        if refused > 0 && !include_refused {
            format!("; {refused} more are in use or kept (include_refused shows why)")
        } else {
            String::new()
        }
    );
    let session = &view["session_cache"];
    let mut outcome = Outcome::new(
        summary,
        json!({
            "revision": view["revision"],
            "volumes": volume_rows,
            "warnings": warnings,
            "categories": categories,
            "session_cache": pick(session, &["bytes", "session_bytes", "sessions", "budget_bytes", "over_budget"]),
            "models": models,
            "items": page(rows, args.cursor.unwrap_or(0), STORAGE_PAGE),
            "truncated": view["truncated"],
        }),
    )
    .next(&["juiced_delete_files", "juiced_remove_model", "juiced_forget_sessions"]);
    if legacy_bytes > 0 {
        outcome = outcome.warn(format!(
            "v0.9.2 leftovers hold {}. Delete them only when the operator names them; they are what a rollback to v0.9.2 uses.",
            size(legacy_bytes)
        ));
    }
    if view["truncated"] == true {
        outcome =
            outcome.warn("Some folders were too large to measure fully; sizes are lower bounds.");
    }
    Ok(outcome)
}
