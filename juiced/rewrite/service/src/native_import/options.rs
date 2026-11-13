use super::{SettingRow, backend_text, display_key, scalar, unsupported_reason};
use crate::{
    engines::catalog::Engine,
    native_settings::Document,
    profiles::LaunchOptions,
    runtime_arguments::{Passthrough, passthrough},
};
use serde_json::{Value, value::RawValue};
use std::collections::BTreeMap;

const MAX_ARGUMENT_TOKENS: usize = 512;

/// One native option layer, lowest precedence first: model image defaults, the registration's
/// own `recipe_options`, then the saved row from `recipe_options.json`.
pub struct Layer<'a> {
    pub scope: &'static str,
    pub values: Vec<(&'a str, &'a RawValue)>,
}

#[derive(Clone, Debug, Default)]
pub struct Mapped {
    pub options: LaunchOptions,
    pub backend: Option<String>,
    pub pinned: bool,
    /// Extra worker arguments in native order: mapped option flags, then passed-through
    /// operator arguments.
    pub arguments: Vec<Vec<String>>,
    /// Native `ctx_size` was absent or `-1`, so the context is sized automatically.
    pub automatic_context: bool,
    /// The backend whose global `<backend>_args` replaced the engine's global `args`.
    pub args_backend: Option<String>,
    pub rows: Vec<SettingRow>,
}

/// Splits native custom argument text exactly like native `parse_custom_args`: spaces separate
/// words, and single or double quotes group text without escapes.
pub fn shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in text.chars() {
        match quote {
            None if character == '"' || character == '\'' => quote = Some(character),
            Some(open) if character == open => quote = None,
            None if character == ' ' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn flag_like(token: &str) -> bool {
    token.starts_with('-') && token.len() > 1 && token.parse::<f64>().is_err()
}

fn row(
    scope: &'static str,
    key: impl Into<String>,
    state: &'static str,
    value: Option<Value>,
    target: Option<&'static str>,
    reason: &'static str,
) -> SettingRow {
    SettingRow {
        scope,
        key: key.into(),
        state,
        value,
        target,
        reason,
    }
}

fn text(value: &RawValue) -> Option<String> {
    serde_json::from_str(value.get()).ok()
}

fn number(value: &RawValue) -> Option<f64> {
    serde_json::from_str::<f64>(value.get())
        .ok()
        .or_else(|| text(value)?.trim().parse().ok())
}

fn whole(value: &RawValue) -> Option<i64> {
    number(value)
        .filter(|value| value.fract() == 0.0 && value.abs() < 1e15)
        .map(|value| value as i64)
}

/// Passes operator arguments through when the selected adapter defines them, the way native
/// appends them after its managed flags. Typed llama.cpp flags become launch options, flags the
/// service owns are dropped, and unknown or authority-changing flags are dropped and listed.
fn passthrough_arguments(
    engine: Engine,
    scope: &'static str,
    key: &str,
    value: &RawValue,
    mapped: &mut Mapped,
) {
    let Some(source) = text(value) else {
        mapped.rows.push(row(
            scope,
            display_key(key),
            "unsupported",
            None,
            None,
            "Engine arguments must be text.",
        ));
        return;
    };
    let adapter = engine.provider();
    let tokens: Vec<String> = shell_words(&source)
        .into_iter()
        .take(MAX_ARGUMENT_TOKENS)
        .collect();
    let mut dropped = Vec::new();
    let mut managed = Vec::new();
    let mut passed = 0usize;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let (flag, inline) = token
            .split_once('=')
            .filter(|(flag, _)| flag.starts_with("--"))
            .map_or((token.as_str(), None), |(flag, value)| (flag, Some(value)));
        if !flag_like(flag) {
            index += 1;
            continue;
        }
        let next = || {
            inline.map(str::to_owned).or_else(|| {
                tokens
                    .get(index + 1)
                    .filter(|next| !flag_like(next))
                    .cloned()
            })
        };
        let width = |value: &Option<String>| match (inline, value) {
            (Some(_), _) => 1,
            (None, Some(_)) => 2,
            (None, None) => 1,
        };
        if engine == Engine::FastFlowLm {
            let value = next();
            match crate::flm::argument(flag, value.as_deref()) {
                Ok(crate::flm::Arity::Flag) if inline.is_none() => {
                    mapped.arguments.push(vec![flag.to_owned()]);
                    passed += 1;
                    index += 1;
                }
                Ok(crate::flm::Arity::Value) => {
                    let value = value.unwrap_or_default();
                    index += width(&Some(value.clone()));
                    mapped.arguments.push(vec![flag.to_owned(), value]);
                    passed += 1;
                }
                _ => {
                    if flag.len() <= 64 {
                        dropped.push(Value::String(flag.to_owned()));
                    }
                    index += width(&value);
                }
            }
            continue;
        }
        if engine == Engine::LlamaCpp
            && let Some(field) = crate::profiles::option_field(flag)
                .filter(|field| crate::profiles::legacy_field(*field))
        {
            let value = next();
            let consumed = width(&value);
            match value.as_deref().and_then(|value| value.parse::<u32>().ok()) {
                Some(number) if field != 0 || number > 0 => {
                    let (target, slot) = match field {
                        0 => ("context_tokens", &mut mapped.options.context_tokens),
                        1 => ("threads", &mut mapped.options.threads),
                        2 => ("gpu_layers", &mut mapped.options.gpu_layers),
                        _ => ("parallel_slots", &mut mapped.options.parallel_slots),
                    };
                    *slot = Some(number);
                    if field == 0 {
                        mapped.automatic_context = false;
                    }
                    mapped.rows.push(row(
                        scope,
                        format!("{}:{flag}", display_key(key)),
                        "mapped",
                        Some(Value::from(number)),
                        Some(target),
                        "Mapped to the typed launch option.",
                    ));
                }
                _ => dropped.push(Value::String(flag.to_owned())),
            }
            index += consumed;
            continue;
        }
        match passthrough(adapter, flag) {
            Passthrough::Accepted(0) if inline.is_none() => {
                mapped.arguments.push(vec![flag.to_owned()]);
                passed += 1;
                index += 1;
            }
            Passthrough::Accepted(_) => {
                let value = next();
                let consumed = width(&value);
                match value {
                    Some(value) => {
                        mapped.arguments.push(vec![flag.to_owned(), value]);
                        passed += 1;
                    }
                    None => dropped.push(Value::String(flag.to_owned())),
                }
                index += consumed;
            }
            Passthrough::ManagerOwned | Passthrough::Companion => {
                managed.push(Value::String(flag.to_owned()));
                let value = next();
                index += width(&value);
            }
            Passthrough::Forbidden | Passthrough::Unknown => {
                if flag.len() <= 64 {
                    dropped.push(Value::String(flag.to_owned()));
                }
                let value = next();
                index += width(&value);
            }
        }
    }
    if passed > 0 {
        mapped.rows.push(row(
            scope,
            display_key(key),
            "mapped",
            Some(Value::from(passed)),
            Some("arguments"),
            "Passed to the worker after the service-owned arguments, as native does.",
        ));
    }
    if !managed.is_empty() {
        mapped.rows.push(row(
            scope,
            format!("{}:managed", display_key(key)),
            "ignored",
            Some(Value::Array(managed)),
            None,
            "The service owns the worker host, port, model, companion and prompt-cache paths; these flags were dropped.",
        ));
    }
    if !dropped.is_empty() {
        mapped.rows.push(row(
            scope,
            format!("{}:unsupported", display_key(key)),
            "unsupported",
            Some(Value::Array(dropped)),
            None,
            "These flags are not defined for the selected engine, change unmanaged files or networking, or lack a value. They were dropped; only flag names are shown.",
        ));
    }
}

fn custom_arguments(engine: Engine) -> bool {
    matches!(
        engine,
        Engine::LlamaCpp
            | Engine::StableDiffusionCpp
            | Engine::WhisperCpp
            | Engine::Moonshine
            | Engine::FastFlowLm
    )
}

/// Native stores image defaults loosely typed; the shared validator sees canonical JSON so
/// imported and edited defaults obey the same ranges.
fn image_default(key: &str, value: &RawValue) -> Option<String> {
    let canonical = match key {
        "steps" | "width" | "height" | "seed" => Value::from(whole(value)?),
        "cfg_scale" | "flow_shift" => Value::from(number(value)?),
        _ => Value::String(text(value)?),
    };
    crate::media::image_default_argument(key, &canonical)
}

/// Maps the effective native options into launch options and worker arguments. Native
/// precedence applies per key: a higher layer replaces a lower one. Unknown keys stay in the
/// untouched source and are only listed.
pub fn map_options(
    engine: Option<Engine>,
    recipe: &str,
    global: &Document,
    layers: &[Layer<'_>],
    labels: &[String],
) -> Mapped {
    map_options_for(engine, recipe, global, layers, labels, None)
}

fn nonempty(value: &RawValue) -> bool {
    text(value).is_some_and(|text| !text.trim().is_empty())
}

/// Whether the engine's global section saves arguments for this backend, as native's
/// `<section>.<backend>_args`.
pub fn has_backend_arguments(global: &Document, recipe: &str, backend: &str) -> bool {
    global
        .row(&recipe.replace('-', ""))
        .and_then(|row| serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(row.get()).ok())
        .and_then(|section| {
            section
                .get(&format!("{backend}_args"))
                .map(|value| nonempty(value))
        })
        .unwrap_or(false)
}

/// [`map_options`] for a known launch backend: native replaces the engine's global `args` with
/// the non-empty `<backend>_args` of the backend it runs. Without one, the native backend
/// preference selects it.
pub fn map_options_for(
    engine: Option<Engine>,
    recipe: &str,
    global: &Document,
    layers: &[Layer<'_>],
    labels: &[String],
    backend: Option<&str>,
) -> Mapped {
    let mut mapped = Mapped {
        automatic_context: true,
        ..Mapped::default()
    };
    let section = recipe.replace('-', "");
    let backend_key = format!("{recipe}_backend");
    let args_key = format!("{section}_args");
    let mut effective: BTreeMap<&str, (&'static str, &RawValue)> = BTreeMap::new();
    let global_section = global
        .row(&section)
        .and_then(|row| serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(row.get()).ok());
    if let Some(value) = global.row("ctx_size") {
        effective.insert("ctx_size", ("global", value));
    }
    if let Some(section) = &global_section {
        if let Some(value) = section.get("backend") {
            effective.insert("__backend", ("global", value.as_ref()));
        }
        if let Some(value) = section.get("args") {
            effective.insert("__args", ("global", value.as_ref()));
        }
    }
    if engine == Some(Engine::StableDiffusionCpp)
        && let Some(section) = &global_section
    {
        for (name, _) in crate::media::IMAGE_DEFAULTS {
            if let Some(value) = section.get(*name) {
                effective.insert(name, ("global", value.as_ref()));
            }
        }
    }
    for layer in layers {
        for (key, value) in &layer.values {
            effective.insert(key, (layer.scope, value));
        }
    }
    if let Some((scope, value)) = effective.remove(backend_key.as_str()) {
        effective.insert("__backend", (scope, value));
    }
    let mut global_args_key = "args".to_owned();
    let args_backend = backend.map(str::to_owned).or_else(|| {
        effective
            .get("__backend")
            .and_then(|(_, value)| backend_text(value))
            .filter(|backend| !matches!(backend.as_str(), "" | "auto" | "system"))
    });
    if let (Some(section), Some(name)) = (&global_section, args_backend) {
        let key = format!("{name}_args");
        if let Some(value) = section.get(&key).filter(|value| nonempty(value)) {
            effective.insert("__args", ("global", value.as_ref()));
            global_args_key = key;
            mapped.args_backend = Some(name);
        }
    }
    let hybrid = effective
        .get("__backend")
        .and_then(|(_, value)| backend_text(value))
        .is_some_and(|backend| backend == "hybrid-rocm");
    let mut image_arguments = Vec::new();
    let mut seen_args = None;
    let mut merge_args = true;
    for (key, (scope, value)) in &effective {
        let (scope, value) = (*scope, *value);
        match *key {
            // Native flattens the engine's global args into the model's `<section>_args`
            // option, so a model that saves its own replaces them.
            "__args" if effective.contains_key(args_key.as_str()) => {}
            "__args" => {
                if let Some(engine) = engine.filter(|engine| custom_arguments(*engine)) {
                    passthrough_arguments(engine, scope, &global_args_key, value, &mut mapped);
                }
            }
            "__backend" => {}
            "ctx_size" => {}
            key if key == args_key => seen_args = Some((scope, value)),
            "pinned" => {
                mapped.pinned = serde_json::from_str::<bool>(value.get()).unwrap_or(false);
                mapped.rows.push(row(
                    scope,
                    "pinned",
                    if mapped.pinned { "hint" } else { "ignored" },
                    scalar(value),
                    Some("policy"),
                    if mapped.pinned {
                        "Pinned models without a saved Juiced policy are adopted as Hot."
                    } else {
                        "Not pinned; no policy suggestion."
                    },
                ));
            }
            "merge_args" => {
                merge_args = serde_json::from_str::<bool>(value.get()).unwrap_or(true);
                mapped.rows.push(row(
                    scope,
                    "merge_args",
                    "mapped",
                    scalar(value),
                    Some("arguments"),
                    "Native runtime argument defaults are merged unless this is false.",
                ));
            }
            "llamacpp_device" if engine == Some(Engine::LlamaCpp) => match text(value)
                .filter(|device| {
                    !device.is_empty()
                        && device.len() <= 32
                        && device.bytes().all(|byte| byte.is_ascii_alphanumeric())
                }) {
                Some(device) if hybrid => {
                    mapped.arguments.push(vec![
                        crate::hybrid::DEVICE_FLAG.to_owned(),
                        device.clone(),
                    ]);
                    mapped.rows.push(row(
                        scope,
                        "llamacpp_device",
                        "mapped",
                        Some(Value::String(device)),
                        Some("hybrid_gpu_device"),
                        "Passed as the hybrid GPU device.",
                    ));
                }
                Some(device) => {
                    mapped.arguments.push(vec!["--device".to_owned(), device.clone()]);
                    mapped.rows.push(row(
                        scope,
                        "llamacpp_device",
                        "mapped",
                        Some(Value::String(device)),
                        Some("arguments"),
                        "Passed as --device, as native does.",
                    ));
                }
                None => mapped.rows.push(row(
                    scope,
                    "llamacpp_device",
                    "default",
                    None,
                    None,
                    "No device was selected; the engine default applies.",
                )),
            },
            "llamacpp_hybrid_memory_mib" | "llamacpp_hybrid_min_prompt"
                if engine == Some(Engine::LlamaCpp) =>
            {
                let flag = if *key == "llamacpp_hybrid_memory_mib" {
                    crate::hybrid::MEMORY_FLAG
                } else {
                    crate::hybrid::MIN_PROMPT_FLAG
                };
                match whole(value).filter(|_| hybrid) {
                    Some(number) => {
                        mapped
                            .arguments.push(vec![flag.to_owned(), number.to_string()]);
                        mapped.rows.push(row(
                            scope,
                            display_key(key),
                            "mapped",
                            Some(Value::from(number)),
                            Some("arguments"),
                            "Passed to the hybrid engine.",
                        ));
                    }
                    None => mapped.rows.push(row(
                        scope,
                        display_key(key),
                        "ignored",
                        scalar(value),
                        None,
                        "Hybrid NPU settings apply only with the hybrid-rocm backend.",
                    )),
                }
            }
            "hybrid_copy_gib" | "hybrid_min_output_width" => mapped.rows.push(row(
                scope,
                display_key(key),
                "unsupported",
                scalar(value),
                None,
                "Settings of the retired hybrid recipe have no equivalent; the hybrid-rocm llama.cpp engine uses llamacpp_hybrid_memory_mib.",
            )),
            "asr4all_threads" | "asr4all_commit" | "asr4all_right" | "parakeet_threads"
                if engine.is_some_and(|engine| {
                    engine == Engine::Asr4all && key.starts_with("asr4all")
                        || engine == Engine::Parakeet && key.starts_with("parakeet")
                }) =>
            {
                let flag = match *key {
                    "asr4all_commit" => "--futo-commit",
                    "asr4all_right" => "--futo-right",
                    _ => "--threads",
                };
                match whole(value).filter(|number| (1..=65536).contains(number)) {
                    Some(number) => {
                        mapped
                            .arguments.push(vec![flag.to_owned(), number.to_string()]);
                        mapped.rows.push(row(
                            scope,
                            display_key(key),
                            "mapped",
                            Some(Value::from(number)),
                            Some("arguments"),
                            "Replaces the engine default.",
                        ));
                    }
                    None => mapped.rows.push(row(
                        scope,
                        display_key(key),
                        "unsupported",
                        None,
                        None,
                        "The value is not a positive integer; the engine default applies.",
                    )),
                }
            }
            key if engine == Some(Engine::StableDiffusionCpp)
                && crate::media::IMAGE_DEFAULTS.iter().any(|(name, _)| *name == key) =>
            {
                let flag = crate::media::IMAGE_DEFAULTS
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, flags)| flags[0])
                    .expect("image flag");
                match image_default(key, value) {
                    Some(text) => {
                        image_arguments.push((flag, text));
                        mapped.rows.push(row(
                            scope,
                            display_key(key),
                            "mapped",
                            scalar(value).or_else(|| serde_json::from_str(value.get()).ok()),
                            Some("image_defaults"),
                            "Passed as the worker's default and editable as the model's image_defaults; requests that set it still override it.",
                        ));
                    }
                    None => mapped.rows.push(row(
                        scope,
                        display_key(key),
                        "unsupported",
                        None,
                        None,
                        "The value is outside the supported image range; the engine default applies.",
                    )),
                }
            }
            "soft_pixel_cap" => mapped.rows.push(row(
                scope,
                "soft_pixel_cap",
                "ignored",
                scalar(value),
                None,
                "Edits without a size use the first image's size, scaled to at most 2048 pixels per side.",
            )),
            key => {
                let (state, reason) = unsupported_reason(key).unwrap_or((
                    "unknown",
                    "This option is not recognized. It stays in the unmodified source.",
                ));
                mapped
                    .rows
                    .push(row(scope, display_key(key), state, None, None, reason));
            }
        }
    }
    if let Some((scope, value)) = seen_args {
        match engine {
            Some(engine) if custom_arguments(engine) => {
                passthrough_arguments(engine, scope, &args_key, value, &mut mapped)
            }
            _ => mapped.rows.push(row(
                scope,
                display_key(&args_key),
                "unsupported",
                None,
                None,
                "Custom arguments for this engine are not imported.",
            )),
        }
    }
    for (flag, value) in image_arguments {
        if !mapped.arguments.iter().any(|group| group[0] == flag) {
            mapped.arguments.push(vec![flag.to_owned(), value]);
        }
    }
    if engine == Some(Engine::LlamaCpp)
        && merge_args
        && labels.iter().any(|label| label == "mtp")
        && !mapped
            .arguments
            .iter()
            .any(|group| group[0] == "--spec-type")
    {
        mapped
            .arguments
            .push(vec!["--spec-type".to_owned(), "draft-mtp".to_owned()]);
        mapped.rows.push(row(
            "default",
            "merge_args:mtp",
            "mapped",
            Some(Value::String("draft-mtp".into())),
            Some("arguments"),
            "Native adds --spec-type draft-mtp for models labeled mtp.",
        ));
    }
    match effective.get("ctx_size") {
        Some((scope, value)) => match serde_json::from_str::<i64>(value.get()) {
            Ok(context @ 1..=2_147_483_647) if mapped.automatic_context => {
                mapped.options.context_tokens = Some(context as u32);
                mapped.automatic_context = false;
                mapped.rows.push(row(
                    scope,
                    "ctx_size",
                    "mapped",
                    Some(Value::from(context)),
                    Some("context_tokens"),
                    "Mapped to the launch context budget.",
                ));
            }
            Ok(-1) if mapped.automatic_context => mapped.rows.push(row(
                scope,
                "ctx_size",
                "default",
                Some(Value::from(-1)),
                Some("context_tokens"),
                "Automatic: the model's trained context, halved until the estimate fits the resident budget.",
            )),
            Ok(_) if !mapped.automatic_context => {}
            _ => mapped.rows.push(row(
                scope,
                "ctx_size",
                "unsupported",
                None,
                None,
                "The saved context is not a positive integer or -1; automatic sizing applies.",
            )),
        },
        None if engine == Some(Engine::LlamaCpp) && mapped.automatic_context => {
            mapped.rows.push(row(
                "default",
                "ctx_size",
                "default",
                None,
                Some("context_tokens"),
                "Automatic: the model's trained context, halved until the estimate fits the resident budget.",
            ))
        }
        None => {}
    }
    if let Some((scope, value)) = effective.get("__backend") {
        let key = if *scope == "global" {
            format!("{section}.backend")
        } else {
            backend_key.clone()
        };
        match backend_text(value) {
            Some(backend) if backend.is_empty() || backend == "auto" || backend == "system" => {
                mapped.rows.push(row(
                    scope,
                    key,
                    "default",
                    Some(Value::String(backend)),
                    Some("backend"),
                    "Automatic or system backend selection uses the hardware recommendation.",
                ))
            }
            Some(backend) => {
                mapped.rows.push(row(
                    scope,
                    key,
                    "mapped",
                    Some(Value::String(backend.clone())),
                    Some("backend"),
                    "Preferred when this host offers the backend; otherwise the hardware recommendation applies.",
                ));
                mapped.backend = Some(backend);
            }
            None => mapped.rows.push(row(
                scope,
                key,
                "unsupported",
                None,
                None,
                "The saved backend is not a recognized name.",
            )),
        }
    }
    mapped
}
