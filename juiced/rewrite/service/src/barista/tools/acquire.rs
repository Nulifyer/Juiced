use super::*;
use std::time::{Duration, Instant};

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_search_models",
        group: Group::Acquire,
        title: "Search models",
        description: "Search the recommended catalog, local files and Hugging Face. Each repo lists quants with size, local presence and a fit verdict for this host.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: true,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "query":{"type":"string","maxLength":200,"description":"Words to match. Omit for recommendations."},
                "task":{"type":"string","enum":["chat","embeddings","transcription","tts","image"]},
                "max_bytes":{"type":"integer","minimum":1,"description":"Hide quants larger than this."},
                "limit":{"type":"integer","minimum":1,"maximum":10,"default":5},
                "hub":{"type":"boolean","default":true,"description":"false searches only the catalog and local files."},
                "cursor":{"type":"string","maxLength":256}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_inspect_repo",
        group: Group::Acquire,
        title: "Inspect repository",
        description: "One Hugging Face repo: license, gated status, revision, every quant with files and size, companion files such as mmproj, and fit verdicts.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: true,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["repo"],"properties":{
                "repo":{"type":"string","minLength":3,"maxLength":200,"pattern":"^[^/\\s]+/[^/\\s]+$"}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_fit",
        group: Group::Acquire,
        title: "Estimate fit",
        description: "Memory estimate for a registered model at a context, slots, GPU layers or KV type, against free budget; or a weights-only estimate for a repo quant.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: true,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "model":model_property(),
                "repo":{"type":"string","minLength":3,"maxLength":200},
                "quant":{"type":"string","minLength":1,"maxLength":64},
                "context_tokens":{"type":"integer","minimum":1,"maximum":1_048_576},
                "parallel_slots":{"type":"integer","minimum":1,"maximum":crate::profiles::MAX_PARALLEL_SLOTS},
                "gpu_layers":{"type":"integer","minimum":0,"maximum":1024},
                "kv_cache_type":{"type":"string","enum":["f16","q8_0","q4_0"]}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_list_local_files",
        group: Group::Acquire,
        title: "List local files",
        description: "Model files already on disk with size and the models using them. Each quant's add value is the exact input for juiced_add_model.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "unused":{"type":"boolean","description":"true lists only files no model uses."},
                "query":{"type":"string","maxLength":100},
                "cursor":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_download",
        group: Group::Acquire,
        title: "Download",
        description: "Start downloading a quant or files from a Hugging Face repo into the library. Returns a job; wait with juiced_wait_job.",
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: true,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["repo"],"properties":{
                "repo":{"type":"string","minLength":3,"maxLength":200,"pattern":"^[^/\\s]+/[^/\\s]+$"},
                "quant":{"type":"string","minLength":1,"maxLength":64,"description":"Quant label such as Q4_K_M."},
                "file":{"type":"string","minLength":1,"maxLength":512},
                "files":{"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":512}},
                "revision":{"type":"string","minLength":1,"maxLength":128},
                "expected_generation":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_add_model",
        group: Group::Acquire,
        title: "Add model",
        description: "Register a downloaded file as a model. Pass exactly one of path, artifact or artifact_set from juiced_list_local_files. Reports engine_not_installed with the engine to install.",
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Working,
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
        group: Group::Acquire,
        title: "Install engine",
        description: "Download and install one engine build listed as installable by juiced_list_engines. Returns a job.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: true,
        preview: true,
        support: Support::Partial(
            "installs the builds in today's engine catalog; engines added by helper-engines appear here once that catalog lands",
        ),
        input: || {
            json!({"type":"object","required":["engine","backend"],"properties":{
                "engine":{"type":"string","minLength":1,"maxLength":64},
                "backend":{"type":"string","minLength":1,"maxLength":64},
                "expected_generation":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_wait_job",
        group: Group::Acquire,
        title: "Wait for job",
        description: "Wait on the server, up to max_seconds, for a download or engine job to finish. Returns its state and progress. Do not loop on it more than a few times.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["id"],"properties":{
                "id":{"type":"string","minLength":3,"maxLength":160,"description":"Job id such as pull:... or engine:..."},
                "max_seconds":{"type":"integer","minimum":1,"maximum":60,"default":30}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_cancel_job",
        group: Group::Acquire,
        title: "Cancel job",
        description: "Cancel a running download. Engine installs cannot be cancelled.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["id"],"properties":{
                "id":{"type":"string","minLength":3,"maxLength":160}
            },"additionalProperties":false})
        },
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: Option<String>,
    task: Option<crate::library::Task>,
    max_bytes: Option<u64>,
    limit: Option<u32>,
    hub: Option<bool>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoArgs {
    repo: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FitArgs {
    model: Option<String>,
    repo: Option<String>,
    quant: Option<String>,
    context_tokens: Option<u32>,
    parallel_slots: Option<u32>,
    gpu_layers: Option<u32>,
    kv_cache_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalArgs {
    unused: Option<bool>,
    query: Option<String>,
    cursor: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadArgs {
    repo: String,
    quant: Option<String>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    id: String,
    max_seconds: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    id: String,
}

pub(super) async fn run(context: &Context<'_>, name: &str, args: Value) -> Option<Result<Outcome>> {
    Some(match name {
        "juiced_search_models" => search(context, args).await,
        "juiced_inspect_repo" => inspect_repo(context, args).await,
        "juiced_fit" => fit(context, args).await,
        "juiced_list_local_files" => local(context, args).await,
        "juiced_download" => download(context, args).await,
        "juiced_add_model" => add(context, args).await,
        "juiced_install_engine" => install(context, args).await,
        "juiced_wait_job" => wait(context, args).await,
        "juiced_cancel_job" => cancel(context, args).await,
        _ => return None,
    })
}

/// The host's memory as the console's Find models page sees it, plus the free model budget.
pub(super) struct Host {
    gpu_bytes: Option<u64>,
    ram_bytes: Option<u64>,
    free_budget_bytes: Option<u64>,
}

impl Host {
    pub(super) async fn read(context: &Context<'_>) -> Self {
        let (system, state) = tokio::join!(context.get("system-info"), context.get("juiced/state"));
        let system = system.unwrap_or(Value::Null);
        let gpu_bytes = list(&system["devices"], "gpus")
            .iter()
            .filter_map(|gpu| gpu["vram_total_bytes"].as_u64())
            .max()
            .filter(|bytes| *bytes > 0);
        let ram_bytes = system["devices"]["memory"]["total_bytes"].as_u64();
        let free_budget_bytes = state.ok().and_then(|state| {
            let resources = &state["resources"];
            Some(
                resources["resident_budget_bytes"]
                    .as_u64()?
                    .saturating_sub(resources["reserved_bytes"].as_u64().unwrap_or(0)),
            )
        });
        Self {
            gpu_bytes,
            ram_bytes,
            free_budget_bytes,
        }
    }

    /// The same weights-only rule as the console (file size plus 15%), so both agree.
    pub(super) fn verdict(&self, bytes: Option<u64>) -> Value {
        let Some(bytes) = bytes else {
            return json!({"verdict":"unknown","reason":"size unknown"});
        };
        let needed = bytes + bytes * 15 / 100;
        let verdict = match (self.gpu_bytes, self.ram_bytes) {
            (Some(gpu), _) if needed <= gpu => "fits",
            (Some(_), Some(ram)) if needed <= ram => "partial_offload",
            (None, Some(ram)) if needed <= ram => "cpu_only",
            (_, Some(_)) => "too_big",
            _ => "unknown",
        };
        let mut fit = json!({"verdict":verdict,"needed_bytes":needed,"basis":"weights plus 15%; context memory not included"});
        if let Some(free) = self.free_budget_bytes {
            fit["within_free_budget"] = json!(needed <= free);
        }
        fit
    }
}

fn quant_row(quant: &Value, host: &Host) -> Value {
    let mut row = pick(
        quant,
        &[
            "label",
            "file",
            "bytes",
            "parts",
            "complete",
            "pullable",
            "recommended",
        ],
    );
    if quant["local"]["present"] == true {
        row["downloaded"] = json!(true);
    }
    if !list(quant, "jobs").is_empty() {
        row["downloading"] = json!(true);
    }
    row["fit"] = host.verdict(quant["bytes"].as_u64());
    row
}

const COMMON: &[&str] = &[
    "Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "F16", "BF16", "MXFP4", "IQ4_NL", "Q4_0",
];

fn featured(quants: &[Value], max_bytes: Option<u64>) -> Vec<Value> {
    let small_enough = |quant: &&Value| {
        max_bytes.is_none_or(|maximum| quant["bytes"].as_u64().is_none_or(|bytes| bytes <= maximum))
    };
    let mut picked: Vec<Value> = quants
        .iter()
        .filter(small_enough)
        .filter(|quant| {
            quant["recommended"] == true
                || quant["local"]["present"] == true
                || quant["label"]
                    .as_str()
                    .is_some_and(|label| COMMON.contains(&label.to_uppercase().as_str()))
        })
        .cloned()
        .collect();
    if picked.is_empty() {
        picked = quants
            .iter()
            .filter(small_enough)
            .take(4)
            .cloned()
            .collect();
    }
    picked.sort_by_key(|quant| quant["bytes"].as_u64().unwrap_or(u64::MAX));
    picked.truncate(6);
    picked
}

fn repo_row(entry: &Value) -> Value {
    pick(
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
            "revision",
            "engine",
        ],
    )
}

async fn search_raw(
    context: &Context<'_>,
    query: Option<&str>,
    task: Option<crate::library::Task>,
    limit: u32,
    hub: bool,
    cursor: Option<&str>,
) -> Result<Value> {
    let mut parameters = vec![format!("limit={limit}")];
    if let Some(text) = query.map(str::trim).filter(|text| !text.is_empty()) {
        bounded(text, 1, 200, "query")?;
        parameters.push(format!("q={}", encode(text)));
    }
    if let Some(task) = task {
        let task = serde_json::to_value(task).unwrap_or(Value::Null);
        parameters.push(format!("task={}", task.as_str().unwrap_or("chat")));
    }
    if !hub {
        parameters.push("hub=false".into());
    }
    if let Some(cursor) = cursor {
        bounded(cursor, 1, 256, "cursor")?;
        parameters.push(format!("cursor={}", encode(cursor)));
    }
    context
        .get(&format!("juiced/library/search?{}", parameters.join("&")))
        .await
}

async fn search(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let search: SearchArgs = parse(args)?;
    let size = limit(search.limit, 5, 10, "limit")?;
    let (result, host) = tokio::join!(
        search_raw(
            context,
            search.query.as_deref(),
            search.task,
            size,
            search.hub != Some(false),
            search.cursor.as_deref()
        ),
        Host::read(context),
    );
    let result = result?;
    let models: Vec<Value> = list(&result, "data")
        .iter()
        .map(|entry| {
            let mut row = repo_row(entry);
            let quants = list(entry, "quants");
            row["quants_total"] = json!(quants.len());
            row["quants"] = json!(
                featured(&quants, search.max_bytes)
                    .iter()
                    .map(|quant| quant_row(quant, &host))
                    .collect::<Vec<_>>()
            );
            row
        })
        .collect();
    let summary = if models.is_empty() {
        if result["offline"] == true {
            "No matches; Juiced is offline, so only local files and the catalog were searched."
                .to_owned()
        } else {
            "No matching models.".to_owned()
        }
    } else {
        format!(
            "{} repo(s): {}.",
            models.len(),
            models
                .iter()
                .filter_map(|model| model["repo"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(Outcome::new(
        summary,
        json!({"generation": result["artifact_generation"], "offline": result["offline"], "next_cursor": result["page"]["next_cursor"], "models": models}),
    )
    .next(&["juiced_inspect_repo", "juiced_download"]))
}

async fn find_repo(context: &Context<'_>, repo: &str) -> Result<(Value, Value)> {
    bounded(repo, 3, 200, "repo")?;
    if !repo.contains('/') {
        return Err(Failure::InvalidParams(
            "repo must be publisher/name.".into(),
        ));
    }
    let result = search_raw(context, Some(repo), None, 10, true, None).await?;
    let entry = list(&result, "data")
        .into_iter()
        .find(|entry| entry["repo"].as_str().is_some_and(|known| known.eq_ignore_ascii_case(repo)))
        .ok_or_else(|| {
            Failure::new(
                "repo_not_found",
                format!("{repo} was not found on Hugging Face or in the library. Check the name with juiced_search_models."),
                404,
            )
        })?;
    Ok((entry, result))
}

async fn inspect_repo(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let RepoArgs { repo } = parse(args)?;
    let ((entry, result), host) = {
        let (found, host) = tokio::join!(find_repo(context, &repo), Host::read(context));
        (found?, host)
    };
    let quants = list(&entry, "quants");
    let mut companions = Vec::new();
    let mut rows = Vec::new();
    for quant in &quants {
        let label = quant["label"].as_str().unwrap_or("").to_lowercase();
        let file = quant["file"].as_str().unwrap_or("").to_lowercase();
        if label.contains("mmproj") || file.contains("mmproj") {
            companions.push(pick(quant, &["label", "file", "bytes"]));
        } else {
            rows.push(quant_row(quant, &host));
        }
    }
    let mut data = repo_row(&entry);
    data["generation"] = result["artifact_generation"].clone();
    data["metadata_error"] = entry["metadata_error"].clone();
    data["files_truncated"] = entry["files_truncated"].clone();
    data["companions"] = json!(companions);
    data["quants"] = json!(rows);
    data["chat_template"] = json!(
        "unknown until downloaded; juiced_model_details reports tools, thinking and vision after adding"
    );
    let gated = entry["gated"].as_bool().unwrap_or(false) || entry["gated"].is_string();
    let mut outcome = Outcome::new(
        format!(
            "{}: {} quant(s), license {}{}{}.",
            entry["repo"].as_str().unwrap_or(&repo),
            rows.len(),
            entry["license"].as_str().unwrap_or("unknown"),
            if gated { ", gated" } else { "" },
            if companions.is_empty() {
                ""
            } else {
                ", with companion files"
            },
        ),
        data,
    )
    .next(&["juiced_download", "juiced_fit"]);
    if gated {
        outcome = outcome.warn("The repo is gated: downloads need an accepted license and a Hugging Face token on the server.");
    }
    Ok(outcome)
}

async fn fit(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let fit: FitArgs = parse(args)?;
    if let Some(kind) = &fit.kv_cache_type {
        bounded(kind, 1, 16, "kv_cache_type")?;
    }
    match (&fit.model, &fit.repo) {
        (Some(model), None) => {
            model_id(model)?;
            let mut query = Vec::new();
            for (key, value) in [
                ("context_tokens", fit.context_tokens),
                ("parallel_slots", fit.parallel_slots),
                ("gpu_layers", fit.gpu_layers),
            ] {
                if let Some(value) = value {
                    query.push(format!("{key}={value}"));
                }
            }
            if let Some(kind) = &fit.kv_cache_type {
                query.push(format!("kv_cache_type={}", encode(kind)));
            }
            let path = format!(
                "models/{}/fit{}",
                encode(model),
                if query.is_empty() {
                    String::new()
                } else {
                    format!("?{}", query.join("&"))
                }
            );
            let estimate = context.get(&path).await?;
            let data = pick(
                &estimate,
                &[
                    "fits",
                    "total_bytes",
                    "available_bytes",
                    "budget_bytes",
                    "reservation_bytes",
                    "largest_context_that_fits",
                    "trained_context",
                    "source",
                    "loaded",
                    "warnings",
                    "breakdown",
                ],
            );
            let summary = format!(
                "{} {} at this setting: needs {} of {} available{}.",
                model,
                if estimate["fits"] == true {
                    "fits"
                } else {
                    "does not fit"
                },
                estimate["total_bytes"]
                    .as_u64()
                    .map_or("unknown".into(), size),
                estimate["available_bytes"]
                    .as_u64()
                    .map_or("unknown".into(), size),
                estimate["largest_context_that_fits"]
                    .as_u64()
                    .map_or(String::new(), |tokens| format!(
                        "; largest context that fits is {tokens} tokens"
                    )),
            );
            Ok(Outcome::new(summary, data).next(&["juiced_set_model_settings"]))
        }
        (None, Some(repo)) => {
            let ((entry, _), host) = {
                let (found, host) = tokio::join!(find_repo(context, repo), Host::read(context));
                (found?, host)
            };
            let quants = list(&entry, "quants");
            let chosen: Vec<&Value> = match &fit.quant {
                Some(label) => quants
                    .iter()
                    .filter(|quant| {
                        quant["label"]
                            .as_str()
                            .is_some_and(|known| known.eq_ignore_ascii_case(label))
                    })
                    .collect(),
                None => quants.iter().collect(),
            };
            if chosen.is_empty() {
                return Err(Failure::new(
                    "quant_not_found",
                    format!("{repo} has no quant {}.", fit.quant.unwrap_or_default()),
                    404,
                ));
            }
            let rows: Vec<Value> = chosen
                .iter()
                .take(20)
                .map(|quant| quant_row(quant, &host))
                .collect();
            let mut outcome = Outcome::new(
                format!(
                    "Weights-only estimate for {} quant(s) of {repo}; register the model and call juiced_fit with model for context memory.",
                    rows.len()
                ),
                json!({"repo": repo, "quants": rows}),
            );
            if fit.context_tokens.is_some() {
                outcome = outcome.warn("Context memory cannot be estimated before the model is added; the verdict covers weights only.");
            }
            Ok(outcome.next(&["juiced_download"]))
        }
        _ => Err(Failure::InvalidParams(
            "Pass either model, or repo with an optional quant.".into(),
        )),
    }
}

async fn local(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let filter: LocalArgs = parse(args)?;
    if let Some(query) = &filter.query {
        bounded(query, 0, 100, "query")?;
    }
    let local = context.get("juiced/library/local").await?;
    let words: Vec<String> = filter
        .query
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let groups: Vec<Value> = list(&local, "groups")
        .iter()
        .filter(|group| filter.unused != Some(true) || list(group, "used_by").is_empty())
        .filter(|group| {
            let name = format!(
                "{} {}",
                group["name"].as_str().unwrap_or(""),
                group["repo"].as_str().unwrap_or("")
            )
            .to_lowercase();
            words.iter().all(|word| name.contains(word))
        })
        .map(|group| {
            let mut entry = pick(
                group,
                &[
                    "name",
                    "repo",
                    "publisher",
                    "bytes",
                    "removable_bytes",
                    "used_by",
                    "read_only",
                    "location",
                ],
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
    let matched = groups.len();
    Ok(Outcome::new(
        format!("{} local model group(s), {} on disk.", matched, local["total_bytes"].as_u64().map_or("unknown".into(), size)),
        json!({"generation": local["artifact_generation"], "total_bytes": local["total_bytes"], "removable_bytes": local["removable_bytes"], "truncated": local["truncated"], "groups": page(groups, filter.cursor.unwrap_or(0), 12)}),
    )
    .next(&["juiced_add_model"]))
}

async fn download(context: &Context<'_>, args: Value) -> Result<Outcome> {
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
    let chosen = [
        pull.quant.is_some(),
        pull.file.is_some(),
        pull.files.is_some(),
    ]
    .iter()
    .filter(|set| **set)
    .count();
    if chosen > 1 {
        return Err(Failure::InvalidParams(
            "Pass one of quant, file or files.".into(),
        ));
    }
    let needs_lookup =
        pull.quant.is_some() || context.preview || pull.expected_generation.is_none();
    let (mut file, mut files) = (pull.file.clone(), pull.files.clone());
    let mut generation = pull.expected_generation;
    let mut planned = json!({"repo": pull.repo});
    if needs_lookup {
        let (entry, result) = find_repo(context, &pull.repo).await?;
        generation = generation.or(result["artifact_generation"].as_u64());
        let host = Host::read(context).await;
        let quants = list(&entry, "quants");
        let quant = if let Some(label) = &pull.quant {
            Some(
                quants
                    .iter()
                    .find(|quant| {
                        quant["label"]
                            .as_str()
                            .is_some_and(|known| known.eq_ignore_ascii_case(label))
                    })
                    .ok_or_else(|| {
                        Failure::new(
                            "quant_not_found",
                            format!(
                                "{} has no quant {label}. Use juiced_inspect_repo.",
                                pull.repo
                            ),
                            404,
                        )
                    })?,
            )
        } else {
            quants.iter().find(|quant| {
                pull.file
                    .as_deref()
                    .is_some_and(|wanted| quant["file"] == wanted)
                    || pull.files.as_ref().is_some_and(|wanted| {
                        list(quant, "files")
                            .iter()
                            .any(|part| wanted.iter().any(|name| part["file"] == name.as_str()))
                    })
            })
        };
        if let Some(quant) = quant {
            if pull.quant.is_some() {
                let parts: Vec<String> = list(quant, "files")
                    .iter()
                    .filter_map(|part| part["file"].as_str().map(str::to_owned))
                    .collect();
                if parts.len() > 1 {
                    files = Some(parts);
                } else {
                    file = quant["file"].as_str().map(str::to_owned);
                }
            }
            planned = json!({
                "repo": pull.repo, "quant": quant["label"], "bytes": quant["bytes"], "parts": quant["parts"],
                "already_downloaded": quant["local"]["present"] == true, "fit": host.verdict(quant["bytes"].as_u64()),
            });
        }
    }
    if file.is_none() && files.is_none() && entry_is_ambiguous(&pull) {
        return Err(Failure::InvalidParams(
            "Name a quant, file or files to download.".into(),
        ));
    }
    if context.preview {
        let mut outcome = Outcome::new(
            format!(
                "Would download {} from {} ({}).",
                planned["quant"]
                    .as_str()
                    .or(file.as_deref())
                    .unwrap_or("the selected files"),
                pull.repo,
                planned["bytes"]
                    .as_u64()
                    .map_or("size unknown".into(), size),
            ),
            json!({"plan": planned, "file": file, "files": files}),
        );
        if planned["already_downloaded"] == true {
            outcome = outcome.warn("This quant is already on disk; downloading reuses it.");
        }
        outcome =
            outcome.warn("Free disk space is not checked here yet; juiced_storage is pending.");
        return Ok(outcome);
    }
    let mut body = json!({"repo": pull.repo, "expected_generation": generation.unwrap_or(0)});
    if let Some(file) = &file {
        body["file"] = json!(file);
    }
    if let Some(files) = &files {
        body["files"] = json!(files);
    }
    if let Some(revision) = &pull.revision {
        body["revision"] = json!(revision);
    }
    let started = context.post("juiced/library/pull", body).await?;
    let job = pull_job(&started);
    Ok(Outcome::new(
        format!(
            "Download started: {}.",
            job["label"].as_str().unwrap_or(&pull.repo)
        ),
        json!({"pull": started["pull"], "plan": planned}),
    )
    .change(&pull.repo, "download", json!(null), job["state"].clone())
    .job(job)
    .next(&["juiced_wait_job", "juiced_add_model"]))
}

fn entry_is_ambiguous(pull: &DownloadArgs) -> bool {
    pull.quant.is_none() && pull.file.is_none() && pull.files.is_none()
}

pub(super) fn pull_job(pull: &Value) -> Value {
    let parts = list(pull, "files");
    let total: u64 = parts.iter().filter_map(|part| part["bytes"].as_u64()).sum();
    let done: u64 = parts
        .iter()
        .map(|part| {
            if part["state"] == "present" {
                part["bytes"].as_u64().unwrap_or(0)
            } else {
                part["downloaded_bytes"].as_u64().unwrap_or(0)
            }
        })
        .sum();
    let state = pull["state"].as_str().unwrap_or("unknown");
    let label = format!(
        "{} {}",
        pull["repo"].as_str().unwrap_or("download"),
        pull["label"]
            .as_str()
            .or(pull["file"].as_str())
            .unwrap_or("")
    );
    json!({
        "id": format!("pull:{}", pull["pull"].as_str().unwrap_or("")),
        "kind": "download", "label": label.trim(), "state": state,
        "done": matches!(state, "complete" | "failed"),
        "progress": if total > 0 { json!(((done as f64 / total as f64) * 1000.0).round() / 1000.0) } else { Value::Null },
        "done_bytes": done, "total_bytes": total, "error": pull["error"],
    })
}

pub(super) fn engine_job(job: &Value) -> Value {
    let state = job["state"].as_str().unwrap_or("unknown");
    let total = job["total_bytes"].as_u64();
    let done = job["downloaded_bytes"].as_u64().unwrap_or(0);
    json!({
        "id": format!("engine:{}", job["id"].as_str().unwrap_or("")),
        "kind": "engine_install",
        "label": format!("{} {}", job["engine"].as_str().unwrap_or("engine"), job["backend"].as_str().unwrap_or("")),
        "state": state, "done": matches!(state, "complete" | "failed"),
        "progress": total.filter(|total| *total > 0).map(|total| ((done as f64 / total as f64) * 1000.0).round() / 1000.0),
        "done_bytes": done, "total_bytes": total, "error": job["error"], "package": job["package"],
    })
}

async fn add(context: &Context<'_>, args: Value) -> Result<Outcome> {
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
        ("path", &add.path),
        ("artifact", &add.artifact),
        ("artifact_set", &add.artifact_set),
        ("id", &add.id),
        ("backend", &add.backend),
    ] {
        if let Some(value) = value {
            body.insert(key.into(), json!(value));
        }
    }
    if let Some(policy) = add.policy {
        body.insert("policy".into(), json!(policy));
    }
    let source = add
        .path
        .as_deref()
        .or(add.artifact.as_deref())
        .or(add.artifact_set.as_deref())
        .unwrap_or("");
    if let Some(id) = &add.id
        && context.model(id).await.is_ok()
    {
        return Err(Failure::new(
            "model_exists",
            format!("A model named {id} is already registered."),
            409,
        ));
    }
    if context.preview {
        let engines = context.get("juiced/engines").await.ok();
        let installed: Vec<Value> = engines
            .as_ref()
            .map(|engines| {
                list(engines, "installed")
                    .iter()
                    .map(|row| row["engine"].clone())
                    .collect()
            })
            .unwrap_or_default();
        return Ok(Outcome::new(
            format!(
                "Would register {} as {} with policy {}.",
                source,
                add.id.as_deref().unwrap_or("a derived id"),
                serde_json::to_value(add.policy.unwrap_or(crate::config::Policy::OnDemand)).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default(),
            ),
            json!({"request": Value::Object(body), "installed_engines": installed}),
        )
        .warn("The engine is chosen when the model is added; it reports engine_not_installed if a build is missing."));
    }
    let added = context
        .post("juiced/models/add", Value::Object(body))
        .await?;
    let model = added.get("model").cloned().unwrap_or(added.clone());
    let id = model["id"]
        .as_str()
        .or(added["id"].as_str())
        .unwrap_or("the model")
        .to_owned();
    Ok(Outcome::new(
        format!("Registered {id}."),
        pick(
            &added,
            &[
                "model",
                "id",
                "policy",
                "revision",
                "engine",
                "backend",
                "implementation_id",
                "warnings",
            ],
        ),
    )
    .change(&id, "registration", json!(null), json!("registered"))
    .next(&["juiced_model_details", "juiced_load", "juiced_test_model"]))
}

async fn install(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let install: EngineArgs = parse(args)?;
    bounded(&install.engine, 1, 64, "engine")?;
    bounded(&install.backend, 1, 64, "backend")?;
    let engines = context.get("juiced/engines").await?;
    let row = list(&engines, "installable").into_iter().find(|row| {
        row["engine"] == install.engine.as_str() && row["backend"] == install.backend.as_str()
    });
    let Some(row) = row else {
        return Err(Failure::new(
            "engine_build_not_listed",
            format!(
                "{} {} is not in the engine catalog for this host. Use juiced_list_engines.",
                install.engine, install.backend
            ),
            404,
        ));
    };
    if context.preview {
        let mut outcome = Outcome::new(
            format!(
                "Would install {} {} ({} download, {}).",
                install.engine,
                install.backend,
                row["build"]["archive_bytes"]
                    .as_u64()
                    .map_or("size unknown".into(), size),
                row["availability"]
                    .as_str()
                    .unwrap_or("unknown availability"),
            ),
            json!({"engine": install.engine, "backend": install.backend, "availability": row["availability"], "download_bytes": row["build"]["archive_bytes"], "release": row["build"]["tag"], "already_installed": row["installed_package"].is_string()}),
        );
        for reason in list(&row, "reasons") {
            if let Some(message) = reason["message"].as_str() {
                outcome = outcome.warn(message.to_owned());
            }
        }
        return Ok(outcome);
    }
    let generation = install
        .expected_generation
        .or(engines["generation"].as_u64())
        .unwrap_or(0);
    let started = context
        .post(
            "juiced/engines/install",
            json!({"engine":install.engine,"backend":install.backend,"expected_generation":generation}),
        )
        .await?;
    if started["already_installed"] == true {
        return Ok(Outcome::new(
            format!(
                "{} {} is already installed.",
                install.engine, install.backend
            ),
            json!({"package": started["package"]}),
        ));
    }
    let job = engine_job(&started["job"]);
    Ok(Outcome::new(
        format!("Installing {} {}.", install.engine, install.backend),
        json!({"engine": install.engine, "backend": install.backend}),
    )
    .change(
        &format!("{} {}", install.engine, install.backend),
        "install",
        json!("not installed"),
        job["state"].clone(),
    )
    .job(job)
    .next(&["juiced_wait_job"]))
}

pub(super) async fn job_view(context: &Context<'_>, id: &str) -> Result<Value> {
    let invalid = || {
        Failure::InvalidParams(
            "Job ids look like pull:<id> or engine:<id>, as tools return them.".into(),
        )
    };
    let (kind, native) = id.split_once(':').ok_or_else(invalid)?;
    bounded(native, 1, 128, "id")?;
    if !native
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid());
    }
    match kind {
        "pull" => Ok(pull_job(
            &context
                .get(&format!("juiced/library/pulls/{native}"))
                .await?,
        )),
        "engine" => {
            let engines = context.get("juiced/engines").await?;
            list(&engines, "jobs")
                .iter()
                .find(|job| job["id"] == native)
                .map(engine_job)
                .ok_or_else(|| {
                    Failure::new(
                        "job_not_found",
                        "No engine job has this id; it may have been dropped after a restart.",
                        404,
                    )
                })
        }
        _ => Err(invalid()),
    }
}

async fn wait(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let wait: WaitArgs = parse(args)?;
    let seconds = limit(wait.max_seconds, 30, 60, "max_seconds")?;
    let deadline = Instant::now() + Duration::from_secs(seconds.into());
    let mut job = job_view(context, &wait.id).await?;
    while job["done"] != true && Instant::now() < deadline {
        tokio::time::sleep(
            Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
        )
        .await;
        if !context.still_authorized().await {
            return Err(Failure::new(
                "assistant_settings_changed",
                "Barista settings changed while waiting. The job keeps running under its own owner.",
                409,
            ));
        }
        job = job_view(context, &wait.id).await?;
    }
    let percent = job["progress"]
        .as_f64()
        .map(|progress| format!(" ({:.0}%)", progress * 100.0))
        .unwrap_or_default();
    let summary = match job["state"].as_str() {
        Some("complete") => format!("{} finished.", job["label"].as_str().unwrap_or("The job")),
        Some("failed") => format!(
            "{} failed: {}.",
            job["label"].as_str().unwrap_or("The job"),
            job["error"].as_str().unwrap_or("unknown error")
        ),
        state => format!(
            "{} is still {}{percent} after {seconds} s. It keeps running; the console shows progress.",
            job["label"].as_str().unwrap_or("The job"),
            state.unwrap_or("running"),
        ),
    };
    let next: &[&str] = match (job["kind"].as_str(), job["state"].as_str()) {
        (Some("download"), Some("complete")) => &["juiced_list_local_files", "juiced_add_model"],
        (Some("engine_install"), Some("complete")) => &["juiced_add_model", "juiced_list_engines"],
        (_, Some("failed")) => &["juiced_logs"],
        _ => &["juiced_wait_job"],
    };
    Ok(Outcome::new(summary, json!({})).job(job).next(next))
}

async fn cancel(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let cancel: CancelArgs = parse(args)?;
    let job = job_view(context, &cancel.id).await?;
    if job["kind"] == "engine_install" {
        return Err(Failure::new(
            "not_supported",
            "Engine installs cannot be cancelled; wait for them to finish.",
            501,
        ));
    }
    if job["done"] == true {
        return Err(Failure::new(
            "job_finished",
            format!(
                "{} already finished.",
                job["label"].as_str().unwrap_or("The job")
            ),
            409,
        ));
    }
    let native = cancel.id.trim_start_matches("pull:");
    let pull = context
        .get(&format!("juiced/library/pulls/{native}"))
        .await?;
    let parts: Vec<String> = list(&pull, "files")
        .iter()
        .filter(|part| part["state"] != "present")
        .filter_map(|part| part["job"].as_str().map(str::to_owned))
        .collect();
    if context.preview {
        return Ok(Outcome::new(
            format!(
                "Would cancel {} part(s) of {}. Finished files stay.",
                parts.len(),
                job["label"].as_str().unwrap_or("the download")
            ),
            json!({"parts": parts.len()}),
        )
        .job(job));
    }
    let mut cancelled = 0;
    for part in &parts {
        let current = context
            .get(&format!("juiced/artifacts/jobs/{part}"))
            .await?;
        if matches!(
            current["state"].as_str(),
            Some("complete" | "cancelled" | "failed")
        ) {
            continue;
        }
        context
            .post(
                &format!("juiced/artifacts/jobs/{part}"),
                json!({"action":"cancel","expected_revision":current["revision"]}),
            )
            .await?;
        cancelled += 1;
    }
    let after = job_view(context, &cancel.id).await.unwrap_or(job.clone());
    Ok(Outcome::new(
        format!(
            "Cancelled {cancelled} part(s) of {}.",
            job["label"].as_str().unwrap_or("the download")
        ),
        json!({}),
    )
    .change(
        &cancel.id,
        "state",
        job["state"].clone(),
        after["state"].clone(),
    )
    .job(after))
}
