use super::*;

const RECLAIM_KINDS: [&str; 4] = ["file", "partial", "engine", "legacy"];

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_remove_model",
        group: Group::Maintain,
        title: "Remove model",
        description: "Remove a Disabled, unloaded model's registration; with delete_files also its weights no other model shares, reporting bytes freed. Aliases pointing at it block removal.",
        read_only: false,
        destructive: true,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["model"],"properties":{
                "model":model_property(),
                "delete_files":{"type":"boolean","default":false,"description":"Also delete the model's files that no other model uses."},
                "expected_revision":{"type":"integer","minimum":0,"description":"The model revision you read."},
                "storage_revision":{"type":"string","minLength":1,"maxLength":64,"description":"With delete_files: the revision from this tool's preview. Required to apply."}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_delete_files",
        group: Group::Maintain,
        title: "Delete files",
        description: "Delete reclaimable items juiced_storage lists: unused files, partial downloads, unused engines, v0.9.2 leftovers. Preview first, then apply with its revision. Juiced rechecks every reference.",
        read_only: false,
        destructive: true,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["items"],"properties":{
                "items":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string","minLength":3,"maxLength":512,"pattern":"^(file|partial|engine|legacy):"},"description":"Item ids from juiced_storage, such as file:... or partial:...."},
                "expected_revision":{"type":"string","minLength":1,"maxLength":64,"description":"The revision from this tool's preview. Required to apply."},
                "legacy":{"type":"boolean","default":false,"description":"Required for legacy: ids. Set it only when the operator asked by name to delete v0.9.2 leftovers."}
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
    storage_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    items: Vec<String>,
    expected_revision: Option<String>,
    #[serde(default)]
    legacy: bool,
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
        "juiced_delete_files" => delete(context, args).await,
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
    if let Some(revision) = &remove.storage_revision {
        storage_revision(revision)?;
        if !remove.delete_files {
            return Err(Failure::InvalidParams(
                "storage_revision goes with delete_files: true.".into(),
            ));
        }
    }
    let model = context.model(&remove.model).await?;
    if let Some(expected) = remove.expected_revision
        && model["revision"].as_u64() != Some(expected)
    {
        return Err(Failure::new(
            "revision_conflict",
            format!("{} changed since you read it. Read it again.", remove.model),
            409,
        ));
    }
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
    if remove.delete_files {
        return remove_with_files(context, &remove, &model, blockers, dependents).await;
    }
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
    .next(&["juiced_storage"]))
}

fn storage_revision(revision: &str) -> Result<()> {
    bounded(revision, 1, 64, "storage revision")?;
    if revision.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(Failure::InvalidParams(
            "The storage revision is the value a preview returned.".into(),
        ))
    }
}

async fn reclaim_preview(context: &Context<'_>, items: &[String]) -> Result<Value> {
    context
        .post(
            "juiced/storage/reclaim",
            json!({"items": items, "preview": true}),
        )
        .await
}

fn revision_required(preview: &Value, field: &str) -> Failure {
    Failure::Route(json!({
        "error": {
            "code": "storage_revision_required",
            "message": format!("Deleting needs the storage revision from a preview. Preview, show the operator what it frees, then pass {field}."),
            "status": 400,
        },
        "revision": preview["revision"],
        "freed_bytes": preview["freed_bytes"],
        "refused": preview["refused"],
    }))
}

fn refusals(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter(|item| item["reclaimable"] != true)
        .map(|item| {
            format!(
                "{}: {}",
                item["title"].as_str().unwrap_or("item"),
                item["message"].as_str().unwrap_or("refused")
            )
        })
        .collect()
}

async fn remove_with_files(
    context: &Context<'_>,
    remove: &RemoveArgs,
    model: &Value,
    blockers: Vec<String>,
    dependents: Vec<Value>,
) -> Result<Outcome> {
    let item = format!("model:{}", remove.model);
    let preview = reclaim_preview(context, std::slice::from_ref(&item)).await?;
    let row = list(&preview, "items")
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    let freed = row["bytes"].as_u64().unwrap_or(0);
    let shared: Vec<Value> = list(&row, "references");
    let shared_note = if shared.is_empty() {
        String::new()
    } else {
        format!(
            " Files it shares with {} stay.",
            shared
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if context.preview {
        let mut outcome = Outcome::new(
            if blockers.is_empty() {
                format!(
                    "Would remove {} and delete its files, freeing {}.{shared_note}",
                    remove.model,
                    size(freed)
                )
            } else {
                format!(
                    "{} cannot be removed yet: {}.",
                    remove.model,
                    blockers.join("; ")
                )
            },
            json!({
                "model": remove.model, "revision": model["revision"], "blockers": blockers,
                "aliases": dependents, "bytes_freed": freed, "shared_with": shared,
                "storage_revision": preview["revision"],
                "apply_with": {"storage_revision": preview["revision"]},
            }),
        );
        if blockers.is_empty() {
            outcome = outcome
                .change(
                    &remove.model,
                    "registration",
                    json!("registered"),
                    json!("removed"),
                )
                .change(
                    &remove.model,
                    "files",
                    json!(format!("{} on disk", size(freed))),
                    json!("deleted"),
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
    let Some(expected) = &remove.storage_revision else {
        return Err(revision_required(&preview, "storage_revision"));
    };
    let applied = context
        .post(
            "juiced/storage/reclaim",
            json!({"items": [item], "expected_revision": expected}),
        )
        .await?;
    let result = list(&applied, "items")
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    let bytes = applied["freed_bytes"].as_u64().unwrap_or(0);
    let gone = context.model(&remove.model).await.is_err();
    if result["applied"] != true {
        return Err(Failure::new(
            result["error"]["code"]
                .as_str()
                .unwrap_or("storage_reclaim_failed"),
            format!(
                "{} was not removed: {}",
                remove.model,
                result["error"]["message"]
                    .as_str()
                    .unwrap_or("the removal failed")
            ),
            409,
        ));
    }
    Ok(Outcome::new(
        format!(
            "Removed {} and freed {}.{shared_note}",
            remove.model,
            size(bytes)
        ),
        json!({"removed": gone, "bytes_freed": bytes, "shared_with": shared}),
    )
    .change(
        &remove.model,
        "registration",
        json!("registered"),
        json!(if gone { "removed" } else { "registered" }),
    )
    .change(
        &remove.model,
        "files",
        json!(format!("{} on disk", size(freed))),
        json!("deleted"),
    )
    .next(&["juiced_storage"]))
}

async fn delete(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let mut request: DeleteArgs = parse(args)?;
    if request.items.is_empty() || request.items.len() > 64 {
        return Err(Failure::InvalidParams(
            "items must list 1 to 64 ids from juiced_storage.".into(),
        ));
    }
    for item in &request.items {
        bounded(item, 3, 512, "item")?;
        let kind = item.split_once(':').map_or("", |(kind, _)| kind);
        if kind == "model" {
            return Err(Failure::InvalidParams(
                "Remove a model and its files with juiced_remove_model and delete_files: true."
                    .into(),
            ));
        }
        if !RECLAIM_KINDS.contains(&kind) {
            return Err(Failure::InvalidParams(format!(
                "{item} is not a juiced_storage item id; ids start with file:, partial:, engine: or legacy:."
            )));
        }
    }
    if let Some(revision) = &request.expected_revision {
        storage_revision(revision)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    request.items.retain(|item| seen.insert(item.clone()));
    let legacy: Vec<&String> = request
        .items
        .iter()
        .filter(|item| item.starts_with("legacy:"))
        .collect();
    if !legacy.is_empty() && !request.legacy {
        return Err(Failure::new(
            "legacy_requires_operator",
            "v0.9.2 leftovers are deleted only when the operator names them, because a rollback to v0.9.2 uses them. Ask the operator, then pass legacy: true.",
            403,
        ));
    }
    let rollback = (!legacy.is_empty())
        .then_some("After this, rolling back to v0.9.2 needs its engines downloaded again.");
    if context.preview {
        let preview = reclaim_preview(context, &request.items).await?;
        let items = list(&preview, "items");
        let refused = refusals(&items);
        let freed = preview["freed_bytes"].as_u64().unwrap_or(0);
        let mut outcome = Outcome::new(
            format!(
                "Would free {} from {} item(s){}.",
                size(freed),
                items.len() - refused.len(),
                if refused.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; {} refused, so applying this set fails until you drop them",
                        refused.len()
                    )
                }
            ),
            json!({
                "revision": preview["revision"], "freed_bytes": freed,
                "items": items.iter().map(inspect::storage_item).collect::<Vec<_>>(),
                "apply_with": {"expected_revision": preview["revision"]},
            }),
        );
        for item in items.iter().filter(|item| item["reclaimable"] == true) {
            outcome = outcome.change(
                item["id"].as_str().unwrap_or("item"),
                item["title"].as_str().unwrap_or("item"),
                json!("on disk"),
                json!("deleted"),
            );
        }
        outcome.warnings.extend(refused);
        if let Some(rollback) = rollback {
            outcome = outcome.warn(rollback);
        }
        return Ok(outcome);
    }
    let Some(expected) = &request.expected_revision else {
        let preview = reclaim_preview(context, &request.items).await?;
        return Err(revision_required(&preview, "expected_revision"));
    };
    let applied = match context
        .post(
            "juiced/storage/reclaim",
            json!({"items": request.items, "expected_revision": expected}),
        )
        .await
    {
        Ok(applied) => applied,
        Err(Failure::Route(mut failure)) if failure["error"]["code"] == "storage_item_refused" => {
            if let Ok(preview) = reclaim_preview(context, &request.items).await {
                failure["refused"] = json!(
                    list(&preview, "items")
                        .iter()
                        .filter(|item| item["reclaimable"] != true)
                        .map(inspect::storage_item)
                        .collect::<Vec<_>>()
                );
            }
            return Err(Failure::Route(failure));
        }
        Err(failure) => return Err(failure),
    };
    let results = list(&applied, "items");
    let freed = applied["freed_bytes"].as_u64().unwrap_or(0);
    let failed: Vec<String> = results
        .iter()
        .filter(|row| row["applied"] != true)
        .map(|row| {
            format!(
                "{}: {}",
                row["title"].as_str().unwrap_or("item"),
                row["error"]["message"].as_str().unwrap_or("not deleted")
            )
        })
        .collect();
    let mut outcome = Outcome::new(
        format!(
            "Freed {} from {} item(s){}.",
            size(freed),
            results.len() - failed.len(),
            if failed.is_empty() {
                String::new()
            } else {
                format!("; {} failed", failed.len())
            }
        ),
        json!({
            "freed_bytes": freed,
            "items": results.iter().map(|row| pick(row, &["id", "kind", "title", "applied", "freed_bytes", "error"])).collect::<Vec<_>>(),
        }),
    )
    .next(&["juiced_storage"]);
    for row in &results {
        outcome = outcome.change(
            row["id"].as_str().unwrap_or("item"),
            row["title"].as_str().unwrap_or("item"),
            json!("on disk"),
            json!(if row["applied"] == true {
                "deleted"
            } else {
                "on disk"
            }),
        );
    }
    outcome.warnings.extend(failed);
    if let Some(rollback) = rollback {
        outcome = outcome.warn(rollback);
    }
    Ok(outcome)
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
