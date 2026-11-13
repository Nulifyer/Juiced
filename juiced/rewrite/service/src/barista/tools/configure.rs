use super::*;

const CACHE_TYPES: [&str; 9] = [
    "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "q5_0", "q5_1", "iq4_nl",
];

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_set_policy",
        group: Group::Configure,
        title: "Set loading policy",
        description: "Set a model to disabled (unloads, refuses requests), on-demand (loads on first request) or hot (loaded at startup and kept loaded; use juiced_load to start it now).",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["model","policy"],"properties":{
                "model":model_property(),
                "policy":{"type":"string","enum":["disabled","on-demand","hot"]},
                "expected_revision":{"type":"integer","minimum":0,"description":"The model revision you read. Omit to use the current one."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_load",
        group: Group::Configure,
        title: "Load model",
        description: "Start a model's worker now with its saved settings. The model must not be disabled.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: model_input,
    },
    Tool {
        name: "juiced_unload",
        group: Group::Configure,
        title: "Unload model",
        description: "Stop a model's worker and free its memory. Hot models must be set to on-demand first.",
        read_only: false,
        destructive: true,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: model_input,
    },
    Tool {
        name: "juiced_set_model_settings",
        group: Group::Configure,
        title: "Set model settings",
        description: "Change one model's saved settings: llama.cpp launch and sampling options with extra_args, image_defaults, or asr_options. Omitted keys keep their value; null returns a key to automatic. Preview shows before -> after and memory fit.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Partial(
            "typed settings exist for llama.cpp models, sd.cpp image defaults and asr4all options; vLLM, DS4, whisper.cpp and the other engines keep the arguments they were added with",
        ),
        input: || {
            let whole = |minimum: i64, maximum: u64, description: &str| json!({"type":["integer","null"],"minimum":minimum,"maximum":maximum,"description":description});
            let decimal = |maximum: f64, description: &str| json!({"type":["number","null"],"minimum":0,"maximum":maximum,"description":description});
            let cache = |description: &str| json!({"type":["string","null"],"enum":CACHE_TYPES.iter().map(|kind| json!(kind)).chain([Value::Null]).collect::<Vec<_>>(),"description":description});
            json!({"type":"object","required":["model","changes"],"properties":{
                "model":model_property(),
                "changes":{"type":"object","additionalProperties":false,"properties":{
                    "context_tokens":whole(1, 1_048_576, "Total context, shared by slots."),
                    "parallel_slots":whole(1, crate::profiles::MAX_PARALLEL_SLOTS.into(), "Concurrent requests."),
                    "gpu_layers":whole(0, 1024, "Layers on the GPU."),
                    "threads":whole(1, 1024, "CPU threads for generation."),
                    "batch_threads":whole(1, 1024, "CPU threads for prompt processing."),
                    "batch_size":whole(1, 1_048_576, "Logical prompt batch."),
                    "ubatch_size":whole(1, 1_048_576, "Physical prompt batch."),
                    "flash_attention":{"type":["string","null"],"enum":["auto","on","off",null]},
                    "cache_type_k":cache("KV cache type for keys; q8_0 halves KV memory."),
                    "cache_type_v":cache("KV cache type for values."),
                    "temperature":decimal(100.0, "Default sampling temperature."),
                    "top_k":whole(0, 1_000_000, "Default top K."),
                    "top_p":decimal(1.0, "Default top P."),
                    "min_p":decimal(1.0, "Default min P."),
                    "repeat_penalty":decimal(100.0, "Default repetition penalty."),
                    "reasoning_budget":whole(-1, 1_000_000, "Thinking tokens; -1 unlimited, 0 off."),
                    "extra_args":{"type":["array","null"],"items":{"type":"string","maxLength":200},"maxItems":32,"description":"Extra engine arguments; null clears them."},
                    "image_defaults":{"type":"object","description":"Image model defaults such as steps or size."},
                    "asr_options":{"type":"object","description":"asr4all speech options threads, commit and right."}
                }},
                "apply":{"type":"string","enum":["next_load","reload"],"default":"next_load"},
                "expected_revision":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_set_cache",
        group: Group::Configure,
        title: "Set cache",
        description: "Change the session cache: mode (off, memory, persistent) globally or for one model, budgets and session count, or explicit snapshots.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "model":model_property(),
                "mode":{"type":["string","null"],"enum":["off","memory","persistent",null],"description":"With model, null returns it to the global mode."},
                "budget_bytes":{"type":"integer","minimum":0},
                "model_budget_bytes":{"type":"integer","minimum":0},
                "max_sessions":{"type":"integer","minimum":0,"maximum":100000},
                "explicit_snapshots":{"type":"boolean"}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_set_protocol",
        group: Group::Configure,
        title: "Set protocol",
        description: "Enable or disable one client protocol: openai, anthropic, ollama, mcp, automatic1111, realtime or wyoming.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["protocol","enabled"],"properties":{
                "protocol":{"type":"string","enum":["openai","anthropic","ollama","mcp","automatic1111","realtime","wyoming"]},
                "enabled":{"type":"boolean"}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_set_alias",
        group: Group::Configure,
        title: "Set alias",
        description: "Point a stable alias name at a model, or remove it with target null. Clients can keep the alias when the model changes.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["alias","target"],"properties":{
                "alias":{"type":"string","minLength":1,"maxLength":256},
                "target":{"type":["string","null"],"maxLength":256},
                "expected_revision":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_set_wyoming",
        group: Group::Configure,
        title: "Set Wyoming",
        description: "Configure the Wyoming voice server for Home Assistant: enabled, speech-to-text model, text-to-speech model and voice, listen address.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "enabled":{"type":"boolean"},
                "stt_model":{"type":["string","null"],"maxLength":256},
                "tts_model":{"type":["string","null"],"maxLength":256},
                "tts_voice":{"type":["string","null"],"maxLength":128},
                "listen":{"type":"string","maxLength":64,"description":"host:port, default 0.0.0.0:10300."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_create_api_key",
        group: Group::Configure,
        title: "Create API key",
        description: "Create an API key. You never see the secret: the operator reveals it once in the chat. Use the narrowest scope (inference for apps).",
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["name","scope"],"properties":{
                "name":{"type":"string","minLength":1,"maxLength":64},
                "scope":{"type":"string","enum":["read","inference","operator"]},
                "expires_in_days":{"type":"integer","minimum":1,"maximum":3650}
            },"additionalProperties":false})
        },
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyArgs {
    model: String,
    policy: crate::config::Policy,
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    model: String,
    changes: Map<String, Value>,
    apply: Option<String>,
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheArgs {
    model: Option<String>,
    #[serde(default, deserialize_with = "explicit")]
    mode: Option<Option<String>>,
    budget_bytes: Option<u64>,
    model_budget_bytes: Option<u64>,
    max_sessions: Option<u32>,
    explicit_snapshots: Option<bool>,
}

fn explicit<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolArgs {
    protocol: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasArgs {
    alias: String,
    target: Option<String>,
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WyomingArgs {
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "explicit")]
    stt_model: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit")]
    tts_model: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit")]
    tts_voice: Option<Option<String>>,
    listen: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    name: String,
    scope: crate::api_keys::Scope,
    expires_in_days: Option<u32>,
}

pub(super) async fn run(context: &Context<'_>, name: &str, args: Value) -> Option<Result<Outcome>> {
    Some(match name {
        "juiced_set_policy" => policy(context, args).await,
        "juiced_load" => load(context, args).await,
        "juiced_unload" => unload(context, args).await,
        "juiced_set_model_settings" => settings(context, args).await,
        "juiced_set_cache" => cache(context, args).await,
        "juiced_set_protocol" => protocol(context, args).await,
        "juiced_set_alias" => alias(context, args).await,
        "juiced_set_wyoming" => wyoming(context, args).await,
        "juiced_create_api_key" => api_key(context, args).await,
        _ => return None,
    })
}

fn policy_name(policy: crate::config::Policy) -> Value {
    serde_json::to_value(policy).unwrap_or(Value::Null)
}

pub(super) fn own_model_refusal(model: &str, action: &str) -> Failure {
    Failure::new(
        "assistant_own_model",
        format!(
            "{model} runs Barista itself, so Barista cannot {action} it. Change it from Models, or pick another Barista model in its settings."
        ),
        409,
    )
}

async fn free_budget(context: &Context<'_>) -> Option<u64> {
    let state = context.get("juiced/state").await.ok()?;
    let resources = &state["resources"];
    Some(
        resources["resident_budget_bytes"]
            .as_u64()?
            .saturating_sub(resources["reserved_bytes"].as_u64().unwrap_or(0)),
    )
}

async fn policy(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let change: PolicyArgs = parse(args)?;
    model_id(&change.model)?;
    if change.policy == crate::config::Policy::Disabled && context.own_model(&change.model) {
        return Err(own_model_refusal(&change.model, "disable"));
    }
    let current = context.model(&change.model).await?;
    let before = current["policy"].clone();
    let after = policy_name(change.policy);
    if context.preview {
        let effect = match change.policy {
            crate::config::Policy::Hot if current["loaded"] != true => {
                let reserve = current["resident_bytes"].as_u64();
                let free = free_budget(context).await;
                json!({"loads_now": false, "loads_at_startup": true, "reserves_bytes": reserve, "free_budget_bytes": free, "fits_budget": reserve.zip(free).map(|(reserve, free)| reserve <= free)})
            }
            crate::config::Policy::Disabled => {
                json!({"unloads": current["loaded"] == true, "requests_refused": true})
            }
            _ => json!({"loads_now": false}),
        };
        return Ok(Outcome::new(
            format!(
                "Would set {} from {} to {}.",
                change.model,
                before.as_str().unwrap_or("?"),
                after.as_str().unwrap_or("?")
            ),
            json!({"model": change.model, "revision": current["revision"], "effect": effect}),
        )
        .change(&change.model, "policy", before, after));
    }
    let model = context
        .post(
            "juiced/policy",
            json!({"model":change.model,"policy":change.policy,"expected_revision":Context::revision(&current, change.expected_revision)}),
        )
        .await?;
    let loading = model["policy"] == "hot" && model["loaded"] != true;
    let mut outcome = Outcome::new(
        format!(
            "{} is now {} ({}){}.",
            change.model,
            model["policy"].as_str().unwrap_or("?"),
            model["state"].as_str().unwrap_or("?"),
            if loading {
                "; it loads in the background, check it with juiced_model_details"
            } else {
                ""
            }
        ),
        json!({"model": inspect::model_row(&model)}),
    )
    .change(&change.model, "policy", before, model["policy"].clone());
    if context.own_model(&change.model) {
        outcome = outcome.warn("This is Barista's own model.");
    }
    Ok(outcome.next(if loading {
        &["juiced_load"]
    } else {
        &["juiced_model_details", "juiced_test_model"]
    }))
}

async fn load(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let args = model_arg(args)?;
    let current = context.model(&args.model).await?;
    if context.preview {
        let reserve = current["resident_bytes"].as_u64();
        let free = free_budget(context).await;
        return Ok(Outcome::new(
            format!(
                "Would load {} ({} reserved, {} free).",
                args.model,
                reserve.map_or("size unknown".into(), size),
                free.map_or("unknown".into(), size)
            ),
            json!({"model": args.model, "loaded": current["loaded"], "policy": current["policy"], "reserves_bytes": reserve, "free_budget_bytes": free}),
        )
        .change(&args.model, "state", current["state"].clone(), json!("ready")));
    }
    let loaded = context.post("load", json!({"model":args.model})).await?;
    let after = context.model(&args.model).await.unwrap_or(Value::Null);
    Ok(Outcome::new(
        format!("{} is {}.", args.model, after["state"].as_str().unwrap_or("loaded")),
        json!({"model": inspect::model_row(&after), "response": pick(&loaded, &["status", "message"])}),
    )
    .change(&args.model, "state", current["state"].clone(), after["state"].clone())
    .next(&["juiced_test_model"]))
}

async fn unload(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let args = model_arg(args)?;
    let current = context.model(&args.model).await?;
    if current["policy"] == "hot" {
        return Err(Failure::new(
            "model_hot",
            format!(
                "{} is Hot, so it stays loaded. Set it to on-demand with juiced_set_policy first.",
                args.model
            ),
            409,
        ));
    }
    if current["loaded"] != true && current["state"] != "loading" {
        return Ok(Outcome::new(
            format!("{} is not loaded.", args.model),
            json!({"model": inspect::model_row(&current)}),
        ));
    }
    if context.preview {
        return Ok(Outcome::new(
            format!("Would unload {} and free about {}.", args.model, current["resident_bytes"].as_u64().map_or("unknown".into(), size)),
            json!({"model": args.model, "loaded": current["loaded"], "active_requests": current["active_requests"], "frees_bytes": current["resident_bytes"]}),
        )
        .change(&args.model, "state", current["state"].clone(), json!("unloaded")));
    }
    context.post("unload", json!({"model":args.model})).await?;
    let after = context.model(&args.model).await.unwrap_or(Value::Null);
    let mut outcome = Outcome::new(
        format!(
            "{} is {}.",
            args.model,
            after["state"].as_str().unwrap_or("unloaded")
        ),
        json!({"model": inspect::model_row(&after)}),
    )
    .change(
        &args.model,
        "state",
        current["state"].clone(),
        after["state"].clone(),
    );
    if context.own_model(&args.model) {
        outcome = outcome.warn("This is Barista's own model; the next reply loads it again.");
    }
    Ok(outcome)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsGroup {
    Launch,
    Image,
    Asr,
}

impl SettingsGroup {
    fn provider(self) -> &'static str {
        match self {
            Self::Launch => "llama-cpp",
            Self::Image => "stable-diffusion-cpp",
            Self::Asr => "asr4all",
        }
    }
    fn what(self) -> &'static str {
        match self {
            Self::Launch => "Launch and sampling settings apply to llama.cpp models",
            Self::Image => "image_defaults apply to sd.cpp image models",
            Self::Asr => "asr_options apply to asr4all speech models",
        }
    }
}

fn settings_group(changes: &Map<String, Value>) -> Result<SettingsGroup> {
    let mut groups = Vec::new();
    for key in changes.keys() {
        let group = match key.as_str() {
            "image_defaults" => SettingsGroup::Image,
            "asr_options" => SettingsGroup::Asr,
            "extra_args" => SettingsGroup::Launch,
            key if crate::profiles::FIELD_KEYS.contains(&key) => SettingsGroup::Launch,
            key => {
                return Err(Failure::InvalidParams(format!(
                    "Unknown setting {key}. Settings: {}, extra_args, image_defaults, asr_options.",
                    crate::profiles::FIELD_KEYS.join(", ")
                )));
            }
        };
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    match groups.as_slice() {
        [group] => Ok(*group),
        [] => Err(Failure::InvalidParams(
            "changes must name at least one setting.".into(),
        )),
        _ => Err(Failure::InvalidParams(
            "Change launch settings (with extra_args), image_defaults and asr_options in separate calls."
                .into(),
        )),
    }
}

fn patch(changes: &Map<String, Value>, key: &str) -> Result<Map<String, Value>> {
    match changes.get(key) {
        Some(Value::Object(patch)) if !patch.is_empty() => Ok(patch.clone()),
        _ => Err(Failure::InvalidParams(format!(
            "{key} must be an object naming at least one option."
        ))),
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => "automatic".into(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.is_empty() => "none".into(),
        other => other.to_string(),
    }
}

async fn settings(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let change: SettingsArgs = parse(args)?;
    model_id(&change.model)?;
    let apply = change.apply.as_deref().unwrap_or("next_load");
    if !matches!(apply, "next_load" | "reload") {
        return Err(Failure::InvalidParams(
            "apply must be next_load or reload.".into(),
        ));
    }
    let group = settings_group(&change.changes)?;
    let id = encode(&change.model);
    let options_path = format!("models/{id}/options");
    let (current, entry) = tokio::join!(context.get(&options_path), context.model(&change.model));
    let (current, entry) = (current?, entry?);
    let provider = current["provider"].as_str().unwrap_or("unknown");
    if provider != group.provider() {
        return Err(Failure::new(
            "settings_unsupported",
            format!(
                "{}; {} runs on {provider}. Use juiced_model_details to see what it accepts.",
                group.what(),
                change.model
            ),
            409,
        ));
    }
    let revision = change
        .expected_revision
        .unwrap_or_else(|| current["revision"].as_u64().unwrap_or(0));
    let reload = apply == "reload" && entry["loaded"] == true;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut data = json!({"model": change.model, "revision": revision, "apply": apply, "loaded": entry["loaded"]});
    let body = match group {
        SettingsGroup::Launch => {
            let saved = current["saved"].as_object().cloned().unwrap_or_default();
            let mut merged = saved.clone();
            let mut launch_changed = false;
            for key in crate::profiles::FIELD_KEYS {
                let Some(value) = change.changes.get(key) else {
                    continue;
                };
                launch_changed = true;
                rows.push(change_row(
                    &change.model,
                    key,
                    saved.get(key).cloned().unwrap_or(Value::Null),
                    value.clone(),
                ));
                if value.is_null() {
                    merged.remove(key);
                } else {
                    merged.insert(key.to_owned(), value.clone());
                }
            }
            let options: crate::profiles::LaunchOptions =
                serde_json::from_value(Value::Object(merged.clone())).map_err(|error| {
                    Failure::InvalidParams(format!(
                        "A setting has the wrong type: {}",
                        error.to_string().chars().take(160).collect::<String>()
                    ))
                })?;
            options
                .validate()
                .map_err(|error| Failure::new(error.code, error.message, 400))?;
            let extra = match change.changes.get("extra_args") {
                None => None,
                Some(Value::Null) => Some(Vec::new()),
                Some(Value::Array(items)) => {
                    let mut extras = Vec::with_capacity(items.len());
                    for item in items {
                        let item = item.as_str().ok_or_else(|| {
                            Failure::InvalidParams("extra_args must be strings.".into())
                        })?;
                        bounded(item, 1, 200, "extra_args item")?;
                        extras.push(item.to_owned());
                    }
                    if extras.len() > 32 {
                        return Err(Failure::InvalidParams("Send at most 32 extra_args.".into()));
                    }
                    Some(extras)
                }
                Some(_) => {
                    return Err(Failure::InvalidParams(
                        "extra_args must be a list of strings or null.".into(),
                    ));
                }
            };
            if let Some(extras) = &extra {
                rows.push(change_row(
                    &change.model,
                    "extra_args",
                    current["extra_args"].clone(),
                    json!(extras),
                ));
            }
            let draft = |key: &str| merged.get(key).and_then(Value::as_u64);
            let mut query: Vec<String> = ["context_tokens", "gpu_layers", "parallel_slots"]
                .iter()
                .filter_map(|key| draft(key).map(|value| format!("{key}={value}")))
                .collect();
            let cache_k = merged.get("cache_type_k").and_then(Value::as_str);
            let cache_v = merged.get("cache_type_v").and_then(Value::as_str);
            match (cache_k, cache_v) {
                (Some(k), Some(v)) if k == v => query.push(format!("kv_cache_type={k}")),
                (None, None) => {}
                _ => warnings.push(
                    "Keys and values use different KV cache types; the memory estimate uses the saved types.".to_owned(),
                ),
            }
            let draft_path = format!("models/{id}/fit?{}", query.join("&"));
            let current_path = format!("models/{id}/fit");
            let (before, after) =
                tokio::join!(context.get(&current_path), context.get(&draft_path));
            match after {
                Ok(after) => {
                    data["memory"] = json!({
                        "before_bytes": before.ok().and_then(|before| before["total_bytes"].as_u64()),
                        "after_bytes": after["total_bytes"],
                        "available_bytes": after["available_bytes"],
                        "fits": after["fits"],
                        "largest_context_that_fits": after["largest_context_that_fits"],
                        "trained_context": after["trained_context"],
                    });
                    if let Some(notes) = after["warnings"].as_array() {
                        warnings.extend(notes.iter().filter_map(Value::as_str).map(str::to_owned));
                    }
                }
                Err(failure) => warnings.push(format!(
                    "No memory estimate: {}",
                    failure.error()["message"].as_str().unwrap_or("unavailable")
                )),
            }
            let mut body = json!({"expected_revision": revision, "apply": apply});
            if launch_changed {
                body["options"] = Value::Object(merged);
            }
            if let Some(extras) = extra {
                body["extra_args"] = json!(extras);
            }
            body
        }
        SettingsGroup::Image | SettingsGroup::Asr => {
            let key = if group == SettingsGroup::Image {
                "image_defaults"
            } else {
                "asr_options"
            };
            let patch = patch(&change.changes, key)?;
            for (field, value) in &patch {
                rows.push(change_row(
                    &change.model,
                    &format!("{key}.{field}"),
                    current[key].get(field).cloned().unwrap_or(Value::Null),
                    value.clone(),
                ));
            }
            json!({"expected_revision": revision, key: patch, "apply": apply})
        }
    };
    let described = rows
        .iter()
        .map(|row| {
            format!(
                "{} {} -> {}",
                row["field"].as_str().unwrap_or("?"),
                text(&row["before"]),
                text(&row["after"])
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    if context.preview {
        let memory = &data["memory"];
        let fit = match memory["fits"].as_bool() {
            Some(true) => format!(
                "; needs {} of {} available",
                memory["after_bytes"].as_u64().map_or("?".into(), size),
                memory["available_bytes"].as_u64().map_or("?".into(), size)
            ),
            Some(false) => format!(
                "; does not fit: needs {}, {} available",
                memory["after_bytes"].as_u64().map_or("?".into(), size),
                memory["available_bytes"].as_u64().map_or("?".into(), size)
            ),
            None => String::new(),
        };
        let summary = format!(
            "Would set {described} for {}{fit}{}.",
            change.model,
            if reload {
                "; the worker restarts"
            } else if entry["loaded"] == true {
                "; applies on the next load"
            } else {
                ""
            },
        );
        let mut outcome = Outcome::new(summary, data);
        outcome.changed = rows;
        outcome.warnings = warnings;
        return Ok(outcome);
    }
    let saved = context.post(&format!("models/{id}/options"), body).await?;
    let after = context
        .get(&format!("models/{id}/options"))
        .await
        .unwrap_or(saved.clone());
    let entry = context.model(&change.model).await.unwrap_or(entry);
    data["saved"] = after["saved"].clone();
    data["effective_launch"] = after["effective_launch"].clone();
    for key in ["extra_args", "image_defaults", "asr_options"] {
        if !after[key].is_null() {
            data[key] = after[key].clone();
        }
    }
    data["context"] = present(&entry["context"]);
    data["state"] = entry["state"].clone();
    if reload {
        let effective = &after["effective_launch"];
        let differs: Vec<&str> = crate::profiles::FIELD_KEYS
            .iter()
            .copied()
            .filter(|key| {
                change
                    .changes
                    .get(*key)
                    .is_some_and(|value| !value.is_null() && effective.get(*key) != Some(value))
            })
            .collect();
        if !differs.is_empty() {
            warnings.push(format!(
                "The running worker reports different values for {}; check juiced_model_details.",
                differs.join(", ")
            ));
        }
    }
    let summary = format!(
        "Saved {described} for {}{}.",
        change.model,
        if reload {
            format!("; reloaded, now {}", entry["state"].as_str().unwrap_or("?"))
        } else {
            "; they apply on the next load".to_owned()
        }
    );
    let mut outcome =
        Outcome::new(summary, data).next(&["juiced_model_details", "juiced_test_model"]);
    outcome.changed = rows;
    outcome.warnings = warnings;
    if context.own_model(&change.model) && reload {
        outcome = outcome.warn("This is Barista's own model; it restarted with the new settings.");
    }
    Ok(outcome)
}

fn change_row(target: &str, field: &str, before: Value, after: Value) -> Value {
    super::change(target, field, before, after)
}

async fn cache(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let change: CacheArgs = parse(args)?;
    if let Some(model) = &change.model {
        model_id(model)?;
    }
    let mode_value = |mode: &str| -> Result<Value> {
        match mode {
            "off" | "memory" | "persistent" => Ok(json!(mode)),
            _ => Err(Failure::InvalidParams(
                "mode must be off, memory or persistent.".into(),
            )),
        }
    };
    let sessions = context.get("juiced/cache/sessions").await?;
    let mut settings = sessions["settings"].clone();
    let mut changes = Vec::new();
    let target = change
        .model
        .clone()
        .unwrap_or_else(|| "session cache".into());
    match (&change.model, &change.mode) {
        (Some(model), Some(mode)) => {
            if !list(&sessions, "models")
                .iter()
                .any(|row| row["model"] == model.as_str())
            {
                return Err(Failure::new(
                    "unknown_model",
                    format!("No model named {model}."),
                    404,
                ));
            }
            let models = settings["models"].as_object().cloned().unwrap_or_default();
            let before = models.get(model).cloned().unwrap_or(Value::Null);
            let mut models = models;
            match mode {
                Some(mode) => {
                    models.insert(model.clone(), mode_value(mode)?);
                }
                None => {
                    models.remove(model);
                }
            }
            changes.push(super::change(
                model,
                "cache_mode",
                before,
                mode.as_deref().map_or(Value::Null, |mode| json!(mode)),
            ));
            settings["models"] = Value::Object(models);
        }
        (None, Some(Some(mode))) => {
            changes.push(super::change(
                &target,
                "mode",
                settings["mode"].clone(),
                mode_value(mode)?,
            ));
            settings["mode"] = json!(mode);
        }
        (None, Some(None)) => {
            return Err(Failure::InvalidParams(
                "The global mode cannot be null.".into(),
            ));
        }
        (Some(_), None) => {
            if change.budget_bytes.is_some()
                || change.model_budget_bytes.is_some()
                || change.max_sessions.is_some()
            {
                return Err(Failure::InvalidParams(
                    "Budgets are global; omit model to change them.".into(),
                ));
            }
        }
        (None, None) => {}
    }
    for (key, value) in [
        ("budget_bytes", change.budget_bytes),
        ("model_budget_bytes", change.model_budget_bytes),
        ("max_sessions", change.max_sessions.map(u64::from)),
    ] {
        if let Some(value) = value {
            changes.push(super::change(
                &target,
                key,
                settings[key].clone(),
                json!(value),
            ));
            settings[key] = json!(value);
        }
    }
    let snapshots = match change.explicit_snapshots {
        Some(enabled) => {
            let view = context.get("juiced/cache").await?;
            changes.push(super::change(
                "explicit snapshots",
                "enabled",
                view["enabled"].clone(),
                json!(enabled),
            ));
            Some((
                enabled,
                view["configuration_generation"].as_u64().unwrap_or(0),
            ))
        }
        None => None,
    };
    if changes.is_empty() {
        return Err(Failure::InvalidParams(
            "Name at least one cache setting to change.".into(),
        ));
    }
    let session_change = changes
        .iter()
        .any(|row| row["target"] != "explicit snapshots");
    if context.preview {
        let mut outcome = Outcome::new(
            format!("Would change {} cache setting(s).", changes.len()),
            json!({"settings": settings}),
        );
        outcome.changed = changes;
        return Ok(outcome);
    }
    if session_change {
        context
            .post(
                "juiced/cache/sessions",
                json!({"action":"configure","expected_generation":sessions["configuration_generation"],"settings":settings}),
            )
            .await?;
    }
    if let Some((enabled, generation)) = snapshots {
        context
            .post(
                "juiced/cache",
                json!({"enabled":enabled,"expected_generation":generation}),
            )
            .await?;
    }
    let after = context.get("juiced/cache/sessions").await?;
    let effective: Vec<Value> = list(&after, "models")
        .iter()
        .filter(|row| {
            change
                .model
                .as_deref()
                .is_none_or(|model| row["model"] == model)
        })
        .map(|row| pick(row, &["model", "mode", "effective", "reason"]))
        .collect();
    let mut outcome = Outcome::new(
        format!("Changed {} cache setting(s).", changes.len()),
        json!({"settings": after["settings"], "models": effective}),
    )
    .next(&["juiced_cache"]);
    outcome.changed = changes;
    Ok(outcome)
}

async fn protocol(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let change: ProtocolArgs = parse(args)?;
    let health = context.get("juiced/protocols/health").await?;
    let before = list(&health, "protocols")
        .iter()
        .find(|row| row["id"] == change.protocol.as_str())
        .map(|row| row["enabled"].clone())
        .ok_or_else(|| Failure::InvalidParams("Unknown protocol.".into()))?;
    if context.preview {
        let mut outcome = Outcome::new(
            format!(
                "Would {} {}.",
                if change.enabled { "enable" } else { "disable" },
                change.protocol
            ),
            json!({"protocol": change.protocol}),
        )
        .change(&change.protocol, "enabled", before, json!(change.enabled));
        if !change.enabled && change.protocol == "openai" {
            outcome =
                outcome.warn("Disabling OpenAI stops the console chat and Barista's own replies.");
        }
        return Ok(outcome);
    }
    if !change.enabled && change.protocol == "openai" {
        return Err(Failure::new(
            "assistant_own_protocol",
            "Barista talks to its model over the OpenAI protocol, so it cannot disable it. Do this from Connect.",
            409,
        ));
    }
    context
        .post(
            "juiced/protocols",
            json!({"protocol":change.protocol,"enabled":change.enabled}),
        )
        .await?;
    let after = context.get("juiced/protocols/health").await?;
    let row = list(&after, "protocols")
        .into_iter()
        .find(|row| row["id"] == change.protocol.as_str())
        .unwrap_or(Value::Null);
    Ok(Outcome::new(
        if row["enabled"] == true {
            format!(
                "{} is enabled ({}).",
                change.protocol,
                row["status"].as_str().unwrap_or("?")
            )
        } else {
            format!("{} is disabled.", change.protocol)
        },
        pick(&row, &["id", "enabled", "status", "check"]),
    )
    .change(&change.protocol, "enabled", before, row["enabled"].clone()))
}

async fn alias(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let change: AliasArgs = parse(args)?;
    bounded(&change.alias, 1, 256, "alias")?;
    if let Some(target) = &change.target {
        model_id(target)?;
    }
    let aliases = context.get("juiced/aliases").await?;
    let before = list(&aliases, "aliases")
        .iter()
        .find(|row| row["alias"] == change.alias.as_str())
        .map(|row| row["target"].clone())
        .unwrap_or(Value::Null);
    let revision = change
        .expected_revision
        .or(aliases["revision"].as_u64())
        .unwrap_or(0);
    if context.preview {
        return Ok(Outcome::new(
            match &change.target {
                Some(target) => format!("Would point {} at {target}.", change.alias),
                None => format!("Would remove alias {}.", change.alias),
            },
            json!({"alias": change.alias, "revision": revision}),
        )
        .change(&change.alias, "target", before, json!(change.target)));
    }
    let after = context
        .post(
            "juiced/aliases",
            json!({"alias":change.alias,"target":change.target,"expected_revision":revision}),
        )
        .await?;
    Ok(Outcome::new(
        match &change.target {
            Some(target) => format!("{} now points at {target}.", change.alias),
            None => format!("Removed alias {}.", change.alias),
        },
        json!({"revision": after["revision"], "aliases": after["aliases"].as_array().map_or(0, Vec::len)}),
    )
    .change(&change.alias, "target", before, json!(change.target)))
}

async fn wyoming(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let change: WyomingArgs = parse(args)?;
    let view = context.get("juiced/wyoming").await?;
    let mut settings = view["settings"].clone();
    let mut changes = Vec::new();
    let mut set = |key: &str, value: Value, settings: &mut Value| {
        if settings[key] != value {
            changes.push(super::change(
                "wyoming",
                key,
                settings[key].clone(),
                value.clone(),
            ));
            settings[key] = value;
        }
    };
    if let Some(enabled) = change.enabled {
        set("enabled", json!(enabled), &mut settings);
    }
    for (key, value, maximum) in [
        ("stt_model", &change.stt_model, 256),
        ("tts_model", &change.tts_model, 256),
        ("tts_voice", &change.tts_voice, 128),
    ] {
        if let Some(value) = value {
            if let Some(text) = value {
                bounded(text, 1, maximum, key)?;
            }
            set(key, json!(value), &mut settings);
        }
    }
    if let Some(listen) = &change.listen {
        bounded(listen, 3, 64, "listen")?;
        set("listen", json!(listen), &mut settings);
    }
    if changes.is_empty() {
        return Ok(Outcome::new(
            "Wyoming already has these settings.",
            json!({"settings": settings}),
        ));
    }
    if context.preview {
        let mut outcome = Outcome::new(
            format!("Would change {} Wyoming setting(s).", changes.len()),
            json!({"settings": settings}),
        );
        outcome.changed = changes;
        if settings["enabled"] == true {
            outcome =
                outcome.warn("Wyoming has no authentication; expose it only on a trusted network.");
        }
        return Ok(outcome);
    }
    let after = context
        .post(
            "juiced/wyoming",
            json!({"settings":settings,"expected_generation":view["configuration_generation"]}),
        )
        .await?;
    let mut outcome = Outcome::new(
        format!(
            "Wyoming is {}{}.",
            if after["settings"]["enabled"] == true { "enabled" } else { "disabled" },
            if after["listener"]["listening"] == true { format!(", listening on {}", after["listener"]["address"].as_str().unwrap_or("its port")) } else { String::new() }
        ),
        json!({"settings": after["settings"], "listener": after["listener"], "resolved": after["resolved"]}),
    )
    .next(&["juiced_test_model", "juiced_connections"]);
    outcome.changed = changes;
    if let Some(error) = after["listener"]["error"].as_str() {
        outcome = outcome.warn(format!("Listener error: {error}"));
    }
    Ok(outcome)
}

async fn api_key(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let request: KeyArgs = parse(args)?;
    bounded(&request.name, 1, 64, "name")?;
    let scope = serde_json::to_value(request.scope).unwrap_or(Value::Null);
    let access = context.get("juiced/access").await?;
    if access["mode"] != "keys" {
        return Err(Failure::new(
            "first_key_from_connect",
            "Juiced is open to this network. The first key turns on keys for every route, including this console, so create it from Connect, where the console adopts it.",
            409,
        ));
    }
    if context.preview {
        return Ok(Outcome::new(
            format!(
                "Would create a {} key named {}.",
                scope.as_str().unwrap_or("?"),
                request.name
            ),
            json!({"keys": list(&access, "keys").len(), "mode": access["mode"], "expires_in_days": request.expires_in_days}),
        ));
    }
    let mut body = json!({"name":request.name,"scope":request.scope});
    if let Some(days) = request.expires_in_days {
        body["expires_in_days"] = json!(days);
    }
    let created = context.post("juiced/access/keys", body).await?;
    let secret = created["created"]["secret"].as_str().map(str::to_owned);
    let key = created["created"]["key"].clone();
    let handle = secret.and_then(|secret| {
        super::super::handles::store(super::super::handles::Value::Secret(secret))
    });
    let mut outcome = Outcome::new(
        format!("Created {} key {}. The operator can reveal the secret once in the chat; you cannot see it.", scope.as_str().unwrap_or("?"), request.name),
        json!({"key": pick(&key, &["id", "name", "scope", "prefix", "created_at_ms", "expires_at_ms"]), "secret_handle": handle, "mode": created["mode"]}),
    )
    .change(&request.name, "api_key", Value::Null, json!("created"));
    if handle.is_none() {
        outcome = outcome.warn("The secret could not be held for reveal; revoke this key from Connect and create another.");
    }
    Ok(outcome)
}
