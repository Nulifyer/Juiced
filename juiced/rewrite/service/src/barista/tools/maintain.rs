use super::*;

const FILES_PENDING: &str =
    "reference-checked file deletion waits on the storage owner (helper-storage)";

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_remove_model",
        group: Group::Maintain,
        title: "Remove model",
        description: "Remove a Disabled, unloaded model's registration. Its files stay on disk. Aliases pointing at it block removal.",
        read_only: false,
        destructive: true,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Partial(
            "removing the registration works; delete_files waits on the storage owner (helper-storage)",
        ),
        input: || {
            json!({"type":"object","required":["model"],"properties":{
                "model":model_property(),
                "delete_files":{"type":"boolean","default":false,"description":"Also delete files no other model uses. Not yet supported."},
                "expected_revision":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_delete_files",
        group: Group::Maintain,
        title: "Delete files",
        description: "Delete model files no registered model, alias or job uses, as listed by juiced_storage. Reports bytes freed.",
        read_only: false,
        destructive: true,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Pending(FILES_PENDING),
        input: || {
            json!({"type":"object","required":["files"],"properties":{
                "files":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string","minLength":1,"maxLength":4096},"description":"File ids from juiced_storage."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_forget_sessions",
        group: Group::Maintain,
        title: "Forget sessions",
        description: "Delete saved session cache entries for one model, one session, or all. Frees cache space; later requests prefill again.",
        read_only: false,
        destructive: true,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "model":model_property(),
                "session":{"type":"string","minLength":1,"maxLength":128},
                "all":{"type":"boolean","description":"true forgets every saved session."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_uninstall_engine",
        group: Group::Maintain,
        title: "Uninstall engine",
        description: "Remove an installed engine build that no model uses. Reports bytes freed.",
        read_only: false,
        destructive: true,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["engine","backend"],"properties":{
                "engine":{"type":"string","minLength":1,"maxLength":64,"description":"Engine as juiced_list_engines names it, such as llama.cpp."},
                "backend":{"type":"string","minLength":1,"maxLength":64}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_pause_service",
        group: Group::Maintain,
        title: "Pause service",
        description: "Pause or resume admission of new inference requests for maintenance. Running requests finish; policies are kept.",
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || json!({"type":"object","required":["paused"],"properties":{"paused":{"type":"boolean"}},"additionalProperties":false}),
    },
    Tool {
        name: "juiced_restart_model",
        group: Group::Maintain,
        title: "Restart model",
        description: "Restart a loaded model's worker with its saved settings, for example after a failure.",
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: model_input,
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArgs {
    model: String,
    #[serde(default)]
    delete_files: bool,
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgetArgs {
    model: Option<String>,
    session: Option<String>,
    all: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineArgs {
    engine: String,
    backend: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseArgs {
    paused: bool,
}

pub(super) async fn run(context: &Context<'_>, name: &str, args: Value) -> Option<Result<Outcome>> {
    Some(match name {
        "juiced_remove_model" => remove(context, args).await,
        "juiced_forget_sessions" => forget(context, args).await,
        "juiced_uninstall_engine" => uninstall(context, args).await,
        "juiced_pause_service" => pause(context, args).await,
        "juiced_restart_model" => restart(context, args).await,
        _ => return None,
    })
}

async fn remove(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let remove: RemoveArgs = parse(args)?;
    model_id(&remove.model)?;
    if context.own_model(&remove.model) {
        return Err(configure::own_model_refusal(&remove.model, "remove"));
    }
    if remove.delete_files {
        return Err(Failure::unsupported(FILES_PENDING));
    }
    let model = context.model(&remove.model).await?;
    let files_path = format!("models/{}/files", encode(&remove.model));
    let (aliases, files) = tokio::join!(context.get("juiced/aliases"), context.get(&files_path));
    let dependents: Vec<Value> = aliases
        .map(|aliases| {
            list(&aliases, "aliases")
                .into_iter()
                .filter(|row| row["target"] == remove.model.as_str())
                .map(|row| row["alias"].clone())
                .collect()
        })
        .unwrap_or_default();
    let kept_bytes: u64 = files
        .map(|files| {
            list(&files, "files")
                .iter()
                .filter_map(|file| file["size_bytes"].as_u64())
                .sum()
        })
        .unwrap_or(0);
    let blockers: Vec<String> = [
        (model["policy"] != "disabled").then(|| {
            format!(
                "it is {}, set it to disabled first",
                model["policy"].as_str().unwrap_or("?")
            )
        }),
        (model["loaded"] == true).then(|| "it is loaded".to_owned()),
        (!dependents.is_empty()).then(|| {
            format!(
                "aliases point at it: {}",
                dependents
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    if context.preview {
        let mut outcome = Outcome::new(
            if blockers.is_empty() {
                format!(
                    "Would remove {}'s registration. Its files ({}) stay on disk.",
                    remove.model,
                    size(kept_bytes)
                )
            } else {
                format!(
                    "{} cannot be removed yet: {}.",
                    remove.model,
                    blockers.join("; ")
                )
            },
            json!({"model": remove.model, "revision": model["revision"], "blockers": blockers, "aliases": dependents, "files_kept_bytes": kept_bytes, "bytes_freed": 0}),
        );
        if blockers.is_empty() {
            outcome = outcome.change(
                &remove.model,
                "registration",
                json!("registered"),
                json!("removed"),
            );
        } else {
            outcome = outcome.next(&["juiced_set_policy", "juiced_set_alias"]);
        }
        return Ok(outcome);
    }
    if !blockers.is_empty() {
        return Err(Failure::new(
            "model_in_use",
            format!(
                "{} cannot be removed: {}.",
                remove.model,
                blockers.join("; ")
            ),
            409,
        ));
    }
    context
        .post("delete", json!({"model":remove.model,"expected_revision":Context::revision(&model, remove.expected_revision)}))
        .await?;
    let gone = context.model(&remove.model).await.is_err();
    Ok(Outcome::new(
        format!(
            "Removed {}. Its files ({}) stay on disk.",
            remove.model,
            size(kept_bytes)
        ),
        json!({"removed": gone, "files_kept_bytes": kept_bytes, "bytes_freed": 0}),
    )
    .change(
        &remove.model,
        "registration",
        json!("registered"),
        json!(if gone { "removed" } else { "registered" }),
    )
    .next(&["juiced_list_local_files"]))
}

async fn forget(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let forget: ForgetArgs = parse(args)?;
    if let Some(model) = &forget.model {
        model_id(model)?;
    }
    if let Some(session) = &forget.session {
        bounded(session, 1, 128, "session")?;
    }
    if forget.model.is_none() && forget.session.is_none() && forget.all != Some(true) {
        return Err(Failure::InvalidParams(
            "Name a model or session, or pass all: true.".into(),
        ));
    }
    let sessions = context.get("juiced/cache/sessions").await?;
    let matching: Vec<Value> = list(&sessions, "sessions")
        .into_iter()
        .filter(|row| {
            forget
                .model
                .as_deref()
                .is_none_or(|model| row["model"] == model)
        })
        .filter(|row| {
            forget
                .session
                .as_deref()
                .is_none_or(|session| row["session"] == session)
        })
        .collect();
    let bytes: u64 = matching
        .iter()
        .filter_map(|row| row["bytes"].as_u64())
        .sum();
    let target = forget
        .session
        .clone()
        .or(forget.model.clone())
        .unwrap_or_else(|| "all sessions".into());
    if context.preview {
        return Ok(Outcome::new(
            format!(
                "Would forget {} saved session(s) and free about {}.",
                matching.len(),
                size(bytes)
            ),
            json!({"sessions": matching.len(), "bytes_freed": bytes}),
        )
        .change(&target, "saved_sessions", json!(matching.len()), json!(0)));
    }
    let mut body = json!({"action":"forget"});
    if let Some(model) = &forget.model {
        body["model"] = json!(model);
    }
    if let Some(session) = &forget.session {
        body["session"] = json!(session);
    }
    let result = context.post("juiced/cache/sessions", body).await?;
    let after = context.get("juiced/cache/sessions").await?;
    let left = list(&after, "sessions")
        .iter()
        .filter(|row| {
            forget
                .model
                .as_deref()
                .is_none_or(|model| row["model"] == model)
        })
        .filter(|row| {
            forget
                .session
                .as_deref()
                .is_none_or(|session| row["session"] == session)
        })
        .count();
    Ok(Outcome::new(
        format!("Forgot {} saved session(s); {} remain for this target.", matching.len().saturating_sub(left), left),
        json!({"result": pick(&result, &["forgotten", "freed_bytes", "sessions"]), "bytes_freed": bytes, "remaining": left}),
    )
    .change(&target, "saved_sessions", json!(matching.len()), json!(left)))
}

fn recipe(engine: &str) -> Option<&'static str> {
    crate::engines::catalog::Engine::all()
        .find(|known| {
            known.name().eq_ignore_ascii_case(engine) || known.recipe().eq_ignore_ascii_case(engine)
        })
        .map(|known| known.recipe())
}

async fn uninstall(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let request: EngineArgs = parse(args)?;
    bounded(&request.engine, 1, 64, "engine")?;
    bounded(&request.backend, 1, 64, "backend")?;
    let recipe = recipe(&request.engine).ok_or_else(|| {
        Failure::new(
            "unknown_engine",
            format!(
                "{} is not an engine. Use juiced_list_engines.",
                request.engine
            ),
            404,
        )
    })?;
    let engines = context.get("juiced/engines").await?;
    let installable = list(&engines, "installable").into_iter().find(|row| {
        row["backend"] == request.backend.as_str() && recipe_matches(&row["engine"], recipe)
    });
    let package = installable
        .as_ref()
        .and_then(|row| row["installed_package"].as_str().map(str::to_owned));
    let installed = list(&engines, "installed").into_iter().find(|row| {
        package
            .as_deref()
            .is_some_and(|package| row["package"] == package)
    });
    let users: Vec<Value> = installed
        .as_ref()
        .map(|row| {
            list(row, "models")
                .iter()
                .filter_map(|model| model.get("model").cloned())
                .collect()
        })
        .unwrap_or_default();
    let bytes = installed
        .as_ref()
        .and_then(|row| row["reserved_bytes"].as_u64());
    let label = format!("{} {}", request.engine, request.backend);
    if context.preview {
        let mut outcome = Outcome::new(
            match (&package, users.is_empty()) {
                (None, _) => format!("{label} is not installed; nothing to remove."),
                (Some(_), true) => format!(
                    "Would remove {label} and free about {}.",
                    bytes.map_or("unknown".into(), size)
                ),
                (Some(_), false) => format!(
                    "{label} is used by {} model(s) and cannot be removed.",
                    users.len()
                ),
            },
            json!({"package": package, "models": users, "bytes_freed": bytes}),
        );
        if package.is_some() && users.is_empty() {
            outcome = outcome.change(&label, "installed", json!(true), json!(false));
        }
        return Ok(outcome);
    }
    let removed = context
        .post(
            "uninstall",
            json!({"recipe":recipe,"backend":request.backend}),
        )
        .await?;
    let freed = removed["freed_bytes"].as_u64().unwrap_or(0);
    let packages = list(&removed, "removed_packages").len();
    Ok(Outcome::new(
        if packages == 0 {
            format!("{label} was not installed.")
        } else {
            format!("Removed {label}, freeing {}.", size(freed))
        },
        json!({"removed_packages": packages, "bytes_freed": freed}),
    )
    .change(&label, "installed", json!(packages > 0), json!(false)))
}

fn recipe_matches(engine: &Value, recipe: &str) -> bool {
    engine.as_str().and_then(self::recipe) == Some(recipe)
}

async fn pause(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let pause: PauseArgs = parse(args)?;
    let admission = context.get("juiced/admission").await?;
    let before = admission["paused"].clone();
    if context.preview {
        return Ok(Outcome::new(
            format!(
                "Would {} admission; {} request(s) are running and finish either way.",
                if pause.paused { "pause" } else { "resume" },
                admission["resources"]["active_requests"]
                    .as_u64()
                    .unwrap_or(0)
            ),
            json!({"active_requests": admission["resources"]["active_requests"]}),
        )
        .change("service", "paused", before, json!(pause.paused)));
    }
    let after = context
        .post("juiced/admission", json!({"paused":pause.paused}))
        .await?;
    let mut outcome = Outcome::new(
        if pause.paused {
            "Admission paused: new inference requests are refused until resumed."
        } else {
            "Admission resumed."
        },
        json!({"paused": after["paused"]}),
    )
    .change("service", "paused", before, after["paused"].clone());
    if pause.paused {
        outcome = outcome
            .warn("Barista's own replies need admission too; resume before asking it for more.");
    }
    Ok(outcome)
}

async fn restart(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let args = model_arg(args)?;
    let model = context.model(&args.model).await?;
    if model["loaded"] != true {
        return Err(Failure::new(
            "model_not_loaded",
            format!("{} is not loaded; use juiced_load.", args.model),
            409,
        ));
    }
    let id = encode(&args.model);
    let options = context.get(&format!("models/{id}/options")).await?;
    let typed = options["provider"] == "llama-cpp";
    if !typed && model["policy"] == "hot" {
        return Err(Failure::new(
            "model_hot",
            format!(
                "{} is Hot and its engine has no in-place restart; set it to on-demand, unload and load it.",
                args.model
            ),
            409,
        ));
    }
    if context.preview {
        return Ok(Outcome::new(
            format!(
                "Would restart {} with its saved settings; {} active request(s).",
                args.model,
                model["active_requests"].as_u64().unwrap_or(0)
            ),
            json!({"method": if typed { "reload saved settings" } else { "unload then load" }}),
        )
        .change(&args.model, "worker", json!("running"), json!("restarted")));
    }
    if typed {
        context
            .post(&format!("models/{id}/options"), json!({"expected_revision":options["revision"],"options":options["saved"],"apply":"reload"}))
            .await?;
    } else {
        context.post("unload", json!({"model":args.model})).await?;
        context.post("load", json!({"model":args.model})).await?;
    }
    let after = context.model(&args.model).await?;
    let mut outcome = Outcome::new(
        format!(
            "Restarted {}; it is {}.",
            args.model,
            after["state"].as_str().unwrap_or("?")
        ),
        json!({"model": inspect::model_row(&after)}),
    )
    .change(
        &args.model,
        "worker",
        model["state"].clone(),
        after["state"].clone(),
    );
    if context.own_model(&args.model) {
        outcome = outcome.warn("This is Barista's own model.");
    }
    Ok(outcome)
}
