use crate::{
    engines::catalog::Engine,
    native_settings::{Document, RecognizedRows, Role, Settings},
    native_store::SourceCohort,
    profiles::LaunchOptions,
    storage::DirectoryReader,
};
use serde::Serialize;
use serde_json::{Value, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

pub const MAX_CANDIDATES: usize = 512;
const MAX_PATH_BYTES: usize = 4096;
const MAX_KEY_BYTES: usize = 128;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
const MAX_CACHE_REPOS: usize = 2048;
const MAX_REPO_FILES: usize = 4096;
const MAX_REPO_DEPTH: usize = 4;
const STATE_DIRECTORY: &str = "juiced";
const STATE_FILE: &str = "state.json";
const CACHE_INDEX: &str = ".lemonade_registry.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    BadRequest,
    Forbidden,
    NotFound,
    Conflict,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub kind: RefusalKind,
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    fn new(kind: RefusalKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect()
}

fn bounded_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= MAX_PATH_BYTES
        && path
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
}

/// Resolves a requested directory to its canonical form and accepts it only beneath a
/// configured import root, so symlinks cannot lead the service outside those roots.
pub fn confine_directory(requested: &Path, roots: &[PathBuf]) -> Result<PathBuf, Refusal> {
    if roots.is_empty() {
        return Err(Refusal::new(
            RefusalKind::Forbidden,
            "import_roots_unconfigured",
            "No import roots are configured. Add the mounted directory to import_roots in the service configuration.",
        ));
    }
    if !bounded_absolute(requested) {
        return Err(Refusal::new(
            RefusalKind::BadRequest,
            "invalid_import_path",
            "Import paths must be bounded absolute server paths.",
        ));
    }
    let canonical = std::fs::canonicalize(requested).map_err(|_| {
        Refusal::new(
            RefusalKind::NotFound,
            "import_path_unavailable",
            "The import directory does not exist or cannot be read by the service.",
        )
    })?;
    if !std::fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(Refusal::new(
            RefusalKind::BadRequest,
            "import_path_not_directory",
            "The import path must name a directory.",
        ));
    }
    if !within(&canonical, &canonical_roots(roots)) {
        return Err(Refusal::new(
            RefusalKind::Forbidden,
            "import_path_outside_roots",
            "The import directory is outside the configured import roots.",
        ));
    }
    Ok(canonical)
}

fn within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn confine_file(path: &Path, roots: &[PathBuf]) -> Option<(PathBuf, u64)> {
    let canonical = std::fs::canonicalize(path).ok()?;
    let metadata = std::fs::metadata(&canonical).ok()?;
    (metadata.is_file() && within(&canonical, roots)).then_some((canonical, metadata.len()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentState {
    Present,
    Absent,
    Damaged,
    Unread,
}

#[derive(Clone, Debug, Serialize)]
pub struct DocumentReport {
    pub file: &'static str,
    pub state: DocumentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub imported: bool,
    pub reason: &'static str,
}

fn source_refusal(error: crate::native_store::Error) -> Refusal {
    use crate::native_store::Reason;
    let file = error
        .role
        .map_or("the native configuration", Role::filename);
    let (code, detail) = match error.reason {
        Reason::Document(reason) => (
            "model_settings_recovery_required",
            document_reason(reason).to_owned(),
        ),
        Reason::UnsupportedSourceGeneration => (
            "configuration_migration_recovery_required",
            "an interrupted native configuration migration marker is present".to_owned(),
        ),
        Reason::CohortTooLarge => (
            "native_metadata_limit",
            "the native documents exceed 64 MiB together".to_owned(),
        ),
        Reason::Cancelled | Reason::Deadline => (
            "native_inspection_interrupted",
            "inspection did not finish within its 30 second bound".to_owned(),
        ),
        Reason::SourceUnavailable => (
            "native_source_unavailable",
            "it is unreadable, not a regular file, or changed while it was read".to_owned(),
        ),
    };
    Refusal::new(
        RefusalKind::Conflict,
        code,
        format!("{file} cannot be imported: {detail}. The source was not modified."),
    )
}

fn document_reason(reason: crate::native_settings::Reason) -> &'static str {
    use crate::native_settings::Reason;
    match reason {
        Reason::FileTooLarge => "it exceeds 16 MiB",
        Reason::InvalidJson => "it is not valid JSON",
        Reason::DuplicateKey => "it repeats a key",
        Reason::DepthLimit => "it nests deeper than 128 levels",
        Reason::EventLimit => "it holds more than 100000 values",
        Reason::NotObject => "its top level is not an object",
        Reason::InvalidRecognizedRow => "a known model row is not an object",
        Reason::InvalidOwnedSettings => "a profile-owned settings row is incomplete or unsupported",
        Reason::ConflictingRows => "two rows claim the same model",
        _ => "its model settings are inconsistent",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Builtin,
    User,
    Extra,
    Unrecognized,
}

#[derive(Clone, Debug, Serialize)]
pub struct SettingRow {
    pub scope: &'static str,
    pub key: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'static str>,
    pub reason: &'static str,
}

/// Native option and profile names are lowercase identifiers; anything else is shown as a
/// placeholder so arbitrary document text never reaches the report.
fn display_key(key: &str) -> String {
    if !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.:-".contains(&byte)
        })
    {
        key.to_owned()
    } else {
        "<unrecognized key>".to_owned()
    }
}

fn scalar(value: &RawValue) -> Option<Value> {
    match serde_json::from_str::<Value>(value.get()).ok()? {
        value @ (Value::Bool(_) | Value::Number(_) | Value::Null) => Some(value),
        _ => None,
    }
}

fn backend_text(value: &RawValue) -> Option<String> {
    let text: String = serde_json::from_str(value.get()).ok()?;
    (text.len() <= 32
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    .then_some(text)
}

#[derive(Clone, Debug, Default)]
pub struct Mapped {
    pub options: LaunchOptions,
    pub backend: Option<String>,
    pub pinned: bool,
    pub rows: Vec<SettingRow>,
}

#[derive(Default)]
struct Layers<'a> {
    context: Option<(&'static str, &'a RawValue)>,
    backend: Option<(&'static str, &'a RawValue)>,
    args: Vec<(&'static str, &'a RawValue)>,
}

fn args_rows(
    scope: &'static str,
    key: &str,
    value: &RawValue,
    options: &mut LaunchOptions,
    rows: &mut Vec<SettingRow>,
) {
    let Ok(text) = serde_json::from_str::<String>(value.get()) else {
        rows.push(SettingRow {
            scope,
            key: display_key(key),
            state: "unsupported",
            value: None,
            target: None,
            reason: "Engine arguments must be text.",
        });
        return;
    };
    let tokens: Vec<&str> = text.split_whitespace().take(256).collect();
    let mut unsupported = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let flag = tokens[index];
        let next = tokens
            .get(index + 1)
            .and_then(|next| next.parse::<u32>().ok());
        let (target, field): (&'static str, Option<&mut Option<u32>>) = match flag {
            "-ngl" | "--n-gpu-layers" | "--gpu-layers" => {
                ("gpu_layers", Some(&mut options.gpu_layers))
            }
            "-t" | "--threads" => ("threads", Some(&mut options.threads)),
            "-c" | "--ctx-size" => ("context_tokens", Some(&mut options.context_tokens)),
            "--jinja" | "--metrics" | "--slots" => ("", None),
            _ => {
                if flag.starts_with('-') && flag.len() <= 64 {
                    unsupported.push(Value::String(flag.to_owned()));
                }
                index += 1;
                continue;
            }
        };
        match (field, next) {
            (None, _) => {
                rows.push(SettingRow {
                    scope,
                    key: format!("{}:{flag}", display_key(key)),
                    state: "ignored",
                    value: None,
                    target: None,
                    reason: "The rewrite llama.cpp profile already passes this flag.",
                });
                index += 1;
            }
            (Some(field), Some(number)) if target != "context_tokens" || number > 0 => {
                *field = Some(number);
                rows.push(SettingRow {
                    scope,
                    key: format!("{}:{flag}", display_key(key)),
                    state: "mapped",
                    value: Some(Value::from(number)),
                    target: Some(target),
                    reason: "Mapped to the typed launch option.",
                });
                index += 2;
            }
            (Some(_), _) => {
                if flag.len() <= 64 {
                    unsupported.push(Value::String(flag.to_owned()));
                }
                index += 1;
            }
        }
    }
    if !unsupported.is_empty() {
        rows.push(SettingRow {
            scope,
            key: display_key(key),
            state: "unsupported",
            value: Some(Value::Array(unsupported)),
            target: None,
            reason: "Custom engine arguments other than GPU layers, threads and context are not imported. Only the flag names are shown.",
        });
    }
}

fn unsupported_reason(key: &str) -> Option<(&'static str, &'static str)> {
    Some(match key {
        "auto_evict" | "evict_idle_timeout" | "downsize_idle_timeout" | "evict_weight_factor" => (
            "unsupported",
            "The rewrite admits and evicts workers by resident memory budget, not native idle eviction.",
        ),
        "auto_update" | "merge_args" => (
            "ignored",
            "The rewrite never updates weights automatically and builds arguments from the typed profile.",
        ),
        "steps" | "cfg_scale" | "width" | "height" | "sampling_method" | "flow_shift" | "seed"
        | "voice" | "speed" | "language" => (
            "unsupported",
            "Request-time generation defaults are not imported; clients send them per request.",
        ),
        _ if key.ends_with("_args") => (
            "unsupported",
            "Custom arguments for this engine are not imported.",
        ),
        _ if key.ends_with("_bin") => (
            "unsupported",
            "Custom engine binaries are not imported; the rewrite installs managed engine packages.",
        ),
        _ => return None,
    })
}

/// Maps the effective native options (global < registration < saved) into the rewrite's typed
/// launch options. Unknown keys stay in the untouched source and are only listed.
pub fn map_options(
    engine: Option<Engine>,
    recipe: &str,
    global: &Document,
    registered: &BTreeMap<String, Box<RawValue>>,
    saved: &crate::native_settings::SavedOptions,
) -> Mapped {
    let mut mapped = Mapped::default();
    let backend_key = format!("{recipe}_backend");
    let args_key = format!("{recipe}_args");
    let mut layers = Layers::default();
    let global_section = global
        .row(&recipe.replace('-', ""))
        .and_then(|row| serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(row.get()).ok());
    if let Some(value) = global.row("ctx_size") {
        layers.context = Some(("global", value));
    }
    if let Some(section) = &global_section {
        if let Some(value) = section.get("backend") {
            layers.backend = Some(("global", value));
        }
        if let Some(value) = section.get("args") {
            layers.args.push(("global", value));
        }
    }
    let mut model_rows: Vec<(&'static str, &str, &RawValue)> = registered
        .iter()
        .map(|(key, value)| ("registration", key.as_str(), value.as_ref()))
        .collect();
    model_rows.extend(saved.iter().map(|(key, value)| ("saved", key, value)));
    let mut seen = BTreeSet::new();
    for (scope, key, value) in model_rows.iter().rev() {
        if !seen.insert(*key) {
            continue;
        }
        match *key {
            "ctx_size" => layers.context = Some((scope, value)),
            "pinned" => {
                mapped.pinned = serde_json::from_str::<bool>(value.get()).unwrap_or(false);
                mapped.rows.push(SettingRow {
                    scope,
                    key: "pinned".into(),
                    state: if mapped.pinned { "hint" } else { "ignored" },
                    value: scalar(value),
                    target: Some("policy"),
                    reason: if mapped.pinned {
                        "Pinned models are suggested as Hot. Every import starts Disabled unless you choose a policy."
                    } else {
                        "Not pinned; no policy suggestion."
                    },
                });
            }
            key if key == backend_key => layers.backend = Some((scope, value)),
            key if key == args_key && engine == Some(Engine::LlamaCpp) => {
                layers.args.push((scope, value))
            }
            key => {
                let (state, reason) = unsupported_reason(key).unwrap_or((
                    "unknown",
                    "This option is not recognized. It stays in the unmodified source.",
                ));
                mapped.rows.push(SettingRow {
                    scope,
                    key: display_key(key),
                    state,
                    value: None,
                    target: None,
                    reason,
                });
            }
        }
    }
    for (scope, value) in layers.args {
        args_rows(
            scope,
            &args_key,
            value,
            &mut mapped.options,
            &mut mapped.rows,
        );
    }
    match layers.context {
        Some((scope, value)) => match serde_json::from_str::<i64>(value.get()) {
            Ok(context @ 1..=2_147_483_647) => {
                mapped.options.context_tokens = Some(context as u32);
                mapped.rows.push(SettingRow {
                    scope,
                    key: "ctx_size".into(),
                    state: "mapped",
                    value: Some(Value::from(context)),
                    target: Some("context_tokens"),
                    reason: "Mapped to the launch context budget.",
                });
            }
            Ok(-1) => mapped.rows.push(SettingRow {
                scope,
                key: "ctx_size".into(),
                state: "default",
                value: Some(Value::from(-1)),
                target: Some("context_tokens"),
                reason: "Native automatic context sizing is replaced by the rewrite's GGUF-derived default.",
            }),
            _ => mapped.rows.push(SettingRow {
                scope,
                key: "ctx_size".into(),
                state: "unsupported",
                value: None,
                target: None,
                reason: "The saved context is not a positive integer or -1; the rewrite default applies.",
            }),
        },
        None if engine == Some(Engine::LlamaCpp) => mapped.rows.push(SettingRow {
            scope: "default",
            key: "ctx_size".into(),
            state: "default",
            value: None,
            target: Some("context_tokens"),
            reason: "No context was saved; the rewrite's GGUF-derived default applies.",
        }),
        None => {}
    }
    if let Some((scope, value)) = layers.backend {
        match backend_text(value) {
            Some(backend) if backend.is_empty() || backend == "auto" => {
                mapped.rows.push(SettingRow {
                    scope,
                    key: backend_key,
                    state: "default",
                    value: Some(Value::String(backend)),
                    target: Some("backend"),
                    reason: "Automatic backend selection uses the rewrite's hardware recommendation.",
                })
            }
            Some(backend) => {
                mapped.rows.push(SettingRow {
                    scope,
                    key: backend_key,
                    state: "mapped",
                    value: Some(Value::String(backend.clone())),
                    target: Some("backend"),
                    reason: "Preferred when this host offers the backend; otherwise the hardware recommendation applies.",
                });
                mapped.backend = Some(backend);
            }
            None => mapped.rows.push(SettingRow {
                scope,
                key: backend_key,
                state: "unsupported",
                value: None,
                target: None,
                reason: "The saved backend is not a recognized name.",
            }),
        }
    }
    mapped
}

const GLOBAL_SERVICE_KEYS: &[&str] = &[
    "host",
    "port",
    "broadcast",
    "log_file",
    "log_level",
    "log_max_file_size_mb",
    "log_max_files",
    "models_dir",
    "allowed_origins",
    "cloud_providers",
    "config_version",
    "_generated",
    "global_timeout",
    "default_model_source",
    "disable_model_filtering",
    "download_rate_limit",
    "no_fetch_executables",
    "auto_check_model_updates",
    "auto_update_models",
    "enable_dgpu_gtt",
];

/// Lists the native global settings with how each one reaches (or does not reach) the
/// rewrite. Values are shown only for scalars, never for text that might carry secrets.
pub fn global_rows(config: &Document) -> Vec<SettingRow> {
    let mut rows = Vec::new();
    for (key, value) in config.iter() {
        let (state, target, reason, show): (&str, Option<&'static str>, &'static str, bool) =
            match key {
                "ctx_size" => (
                    "mapped",
                    Some("context_tokens"),
                    "Used as the context for imported models that saved none; -1 keeps the rewrite default.",
                    true,
                ),
                "max_loaded_models" | "auto_evict" | "auto_evict_threshold_pct" => (
                    "unsupported",
                    None,
                    "The rewrite admits workers by resident memory budget instead of model counts or eviction thresholds.",
                    true,
                ),
                "offline" => (
                    "hint",
                    Some("artifact_source_policy"),
                    "Not applied. Set artifact_source_policy to offline in the rewrite configuration to match.",
                    true,
                ),
                "inhibit_suspend" => (
                    "hint",
                    Some("inhibit_suspend"),
                    "Not applied. The rewrite keeps its own inhibit_suspend setting.",
                    true,
                ),
                "extra_models_dir" => (
                    "unsupported",
                    None,
                    "Extra model directories are not scanned; import those files as user models instead.",
                    false,
                ),
                key if GLOBAL_SERVICE_KEYS.contains(&key) => (
                    "ignored",
                    None,
                    "Service-level setting owned by the container and its environment.",
                    false,
                ),
                key if Engine::parse(key).is_some() => {
                    rows.extend(engine_section_rows(key, value));
                    continue;
                }
                _ if value.get().trim_start().starts_with('{') => (
                    "unsupported",
                    None,
                    "Settings for this engine family are not imported because the rewrite has no such engine.",
                    false,
                ),
                _ => (
                    "unknown",
                    None,
                    "This setting is not recognized. It stays in the unmodified source.",
                    false,
                ),
            };
        rows.push(SettingRow {
            scope: "global",
            key: display_key(key),
            state,
            value: show.then(|| scalar(value)).flatten(),
            target,
            reason,
        });
    }
    rows
}

fn engine_section_rows(section: &str, value: &RawValue) -> Vec<SettingRow> {
    let Ok(entries) = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(value.get()) else {
        return vec![SettingRow {
            scope: "global",
            key: display_key(section),
            state: "unsupported",
            value: None,
            target: None,
            reason: "This engine section is not an object.",
        }];
    };
    entries
        .iter()
        .map(|(key, value)| {
            let name = format!("{}.{}", display_key(section), display_key(key));
            let text: Option<String> = serde_json::from_str(value.get()).ok();
            let (state, target, reason, shown) = match key.as_str() {
                "backend" => (
                    if text.as_deref().is_none_or(|text| text.is_empty() || text == "auto") {
                        "default"
                    } else {
                        "mapped"
                    },
                    Some("backend"),
                    "Preferred for imported models of this engine when the host offers it.",
                    backend_text(value).map(Value::String),
                ),
                "args" if section == "llamacpp" || section == "llama.cpp" => (
                    "mapped",
                    Some("launch options"),
                    "GPU layers, threads and context flags apply to imported models; other flags are reported per model.",
                    None,
                ),
                key if key.ends_with("_bin") && text.as_deref() == Some("builtin") => (
                    "ignored",
                    None,
                    "Built-in engine binaries are replaced by managed rewrite engine packages.",
                    None,
                ),
                _ if text.as_deref() == Some("") => (
                    "ignored",
                    None,
                    "Empty setting.",
                    None,
                ),
                key => {
                    let (state, reason) = unsupported_reason(key).unwrap_or((
                        "unsupported",
                        "This engine setting is not imported.",
                    ));
                    (state, None, reason, None)
                }
            };
            SettingRow {
                scope: "global",
                key: name,
                state,
                value: shown.or_else(|| {
                    (state != "unsupported" && state != "unknown")
                        .then(|| scalar(value))
                        .flatten()
                }),
                target,
                reason,
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct Definition {
    pub recipe: Option<String>,
    pub checkpoint: Option<String>,
    pub mmproj: Option<String>,
    pub other_checkpoints: Vec<String>,
    pub labels: Vec<String>,
    pub source: Option<String>,
    pub registered: BTreeMap<String, Box<RawValue>>,
}

fn definition(row: &RawValue) -> Option<Definition> {
    let value: serde_json::Map<String, Value> = serde_json::from_str(row.get()).ok()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let checkpoints = value.get("checkpoints").and_then(Value::as_object);
    let checkpoint = text("checkpoint")
        .filter(|text| !text.is_empty())
        .or_else(|| {
            checkpoints
                .and_then(|map| map.get("main"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let mmproj = text("mmproj").filter(|text| !text.is_empty()).or_else(|| {
        checkpoints
            .and_then(|map| map.get("mmproj"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let other_checkpoints = checkpoints
        .map(|map| {
            map.keys()
                .filter(|role| !["main", "mmproj", "npu_cache"].contains(&role.as_str()))
                .map(|role| display_key(role))
                .collect()
        })
        .unwrap_or_default();
    let labels = value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(Value::as_str)
                .filter(|label| label.len() <= 64)
                .take(32)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let registered = row_object(row, "recipe_options").unwrap_or_default();
    Some(Definition {
        recipe: text("recipe"),
        checkpoint,
        mmproj,
        other_checkpoints,
        labels,
        source: text("source").filter(|source| !source.is_empty()),
        registered,
    })
}

fn row_object(row: &RawValue, key: &str) -> Option<BTreeMap<String, Box<RawValue>>> {
    let object: BTreeMap<String, Box<RawValue>> = serde_json::from_str(row.get()).ok()?;
    serde_json::from_str(object.get(key)?.get()).ok()
}

fn registry() -> &'static BTreeMap<String, Box<RawValue>> {
    static REGISTRY: std::sync::OnceLock<BTreeMap<String, Box<RawValue>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        serde_json::from_str(crate::native_execution::NATIVE_REGISTRY)
            .expect("compiled native registry is valid JSON")
    })
}

#[derive(Clone, Debug)]
pub struct CacheRepo {
    pub directory: PathBuf,
    pub snapshot: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub native_id: String,
    pub public_name: String,
    pub origin: Origin,
    pub definition: Option<Definition>,
    pub selected_profile: Option<String>,
    pub saved: Option<crate::native_settings::SavedOptions>,
    pub previous_policy: Option<(&'static str, &'static str)>,
    pub downloaded_index: Option<CacheRepo>,
}

pub struct Inspection {
    pub source: PathBuf,
    pub weights_root: Option<PathBuf>,
    pub documents: Vec<DocumentReport>,
    pub config: Document,
    pub aliases: BTreeMap<String, String>,
    pub candidates: Vec<Candidate>,
    pub truncated: bool,
    roots: Vec<PathBuf>,
    _cohort: SourceCohort,
}

fn observe_bounded(directory: &Path, name: &str, maximum: u64) -> Result<Option<Vec<u8>>, ()> {
    let reader = match DirectoryReader::open_existing(directory) {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let observation = reader.observe(name, maximum).map_err(|_| ())?;
    Ok(observation.bytes().map(<[u8]>::to_vec))
}

fn policies(source: &Path) -> (DocumentState, BTreeMap<String, &'static str>) {
    #[derive(serde::Deserialize)]
    struct State {
        #[serde(default)]
        policies: BTreeMap<String, Value>,
    }
    let bytes = match observe_bounded(&source.join(STATE_DIRECTORY), STATE_FILE, MAX_STATE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (DocumentState::Absent, BTreeMap::new()),
        Err(()) => return (DocumentState::Damaged, BTreeMap::new()),
    };
    let Ok(state) = serde_json::from_slice::<State>(&bytes) else {
        return (DocumentState::Damaged, BTreeMap::new());
    };
    let policies = state
        .policies
        .into_iter()
        .take(MAX_CANDIDATES * 2)
        .filter_map(|(name, policy)| {
            let policy = match policy.as_str()? {
                "disabled" => "disabled",
                "on-demand" | "on_demand" | "ondemand" => "on-demand",
                "hot" | "pinned" => "hot",
                _ => return None,
            };
            Some((name, policy))
        })
        .collect();
    (DocumentState::Present, policies)
}

fn aliases(source: &Path) -> (DocumentReport, BTreeMap<String, String>) {
    let mut report = DocumentReport {
        file: "aliases.json",
        state: DocumentState::Absent,
        bytes: None,
        sha256: None,
        imported: false,
        reason: "No native aliases were found beside the configuration or in its sibling cache directory.",
    };
    let candidates = [source.to_owned(), source.with_file_name("cache")];
    for directory in candidates {
        let bytes = match observe_bounded(
            &directory,
            "aliases.json",
            crate::native_settings::MAX_DOCUMENT_BYTES as u64,
        ) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(()) => {
                report.state = DocumentState::Damaged;
                report.reason = "The native aliases file is unreadable or not a regular file. It was not imported or changed.";
                return (report, BTreeMap::new());
            }
        };
        report.bytes = Some(bytes.len());
        match Document::parse_bytes(Role::Aliases, Some(&bytes)) {
            Ok(document) => {
                report.state = DocumentState::Present;
                report.imported = true;
                report.reason = "Aliases whose targets are imported can be added.";
                let entries = document
                    .iter()
                    .filter_map(|(alias, target)| {
                        let target: String = serde_json::from_str(target.get()).ok()?;
                        (crate::config::valid_model_id(alias)
                            && crate::config::valid_model_id(&target)
                            && alias != target)
                            .then(|| (alias.to_owned(), target))
                    })
                    .take(crate::config::MAXIMUM_ALIASES)
                    .collect();
                return (report, entries);
            }
            Err(error) => {
                report.state = DocumentState::Damaged;
                report.reason = document_reason(error.reason);
                return (report, BTreeMap::new());
            }
        }
    }
    (report, BTreeMap::new())
}

/// Locates the Hugging Face hub directory of an old installation: an explicit directory (the
/// hub itself or its parent) or the `huggingface/hub` sibling of the configuration directory.
pub fn weights_root(
    source: &Path,
    explicit: Option<&Path>,
    roots: &[PathBuf],
) -> Result<Option<PathBuf>, Refusal> {
    let candidate = match explicit {
        Some(path) => {
            let path = confine_directory(path, roots)?;
            if path.join("hub").is_dir() {
                path.join("hub")
            } else {
                path
            }
        }
        None => match source.parent() {
            Some(parent) => parent.join("huggingface").join("hub"),
            None => return Ok(None),
        },
    };
    Ok(std::fs::canonicalize(&candidate)
        .ok()
        .filter(|path| path.is_dir() && within(path, &canonical_roots(roots))))
}

fn cache_index(weights: &Path) -> BTreeMap<String, CacheRepo> {
    #[derive(serde::Deserialize)]
    struct Processed {
        #[serde(default)]
        snapshot_id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Index {
        #[serde(default)]
        processed_models: BTreeMap<String, Processed>,
        #[serde(default)]
        snapshot_id: Option<String>,
    }
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(weights) else {
        return found;
    };
    for entry in entries.flatten().take(MAX_CACHE_REPOS) {
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with("models--"))
        {
            continue;
        }
        let directory = entry.path();
        let Ok(Some(bytes)) = observe_bounded(&directory, CACHE_INDEX, MAX_INDEX_BYTES) else {
            continue;
        };
        let Ok(index) = serde_json::from_slice::<Index>(&bytes) else {
            continue;
        };
        for (model, processed) in index.processed_models.into_iter().take(64) {
            found.entry(model).or_insert_with(|| CacheRepo {
                directory: directory.clone(),
                snapshot: processed.snapshot_id.or_else(|| index.snapshot_id.clone()),
            });
        }
    }
    found
}

impl Inspection {
    pub fn open(
        source: &Path,
        weights: Option<&Path>,
        roots: &[PathBuf],
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, Refusal> {
        let source = confine_directory(source, roots)?;
        let weights_root = weights_root(&source, weights, roots)?;
        let cohort =
            SourceCohort::inspect_for_mapping(&source, cancelled).map_err(source_refusal)?;
        let mut documents: Vec<DocumentReport> = cohort
            .files()
            .iter()
            .map(|file| {
                let role = file.document().role();
                let present = !file.document().is_absent();
                let (imported, reason) = match role {
                    Role::Config => (
                        present,
                        "Global defaults are mapped where the rewrite has an equivalent.",
                    ),
                    Role::RecipeOptions | Role::ExecutionProfiles => {
                        (present, "Saved model options are mapped to launch options.")
                    }
                    Role::UserModels => {
                        (present, "User model registrations are offered for import.")
                    }
                    Role::Jobs | Role::NativeHelpers => {
                        (false, "Native jobs and helper records are not replayed.")
                    }
                    Role::Aliases => (false, ""),
                };
                DocumentReport {
                    file: role.filename(),
                    state: if present {
                        DocumentState::Present
                    } else {
                        DocumentState::Absent
                    },
                    bytes: file.document().original_bytes().map(|bytes| bytes.len()),
                    sha256: file.sha256().map(str::to_owned),
                    imported,
                    reason: if present {
                        reason
                    } else {
                        "Absent; native defaults apply."
                    },
                }
            })
            .collect();
        let document = |role: Role| {
            cohort
                .file(role)
                .map(|file| file.document().clone())
                .ok_or_else(|| {
                    source_refusal(crate::native_store::Error {
                        role: Some(role),
                        reason: crate::native_store::Reason::SourceUnavailable,
                    })
                })
        };
        let config = document(Role::Config)?;
        let recipe = document(Role::RecipeOptions)?;
        let profiles = document(Role::ExecutionProfiles)?;
        let users = document(Role::UserModels)?;
        let registry = registry();
        let canonical = registry
            .keys()
            .map(|key| format!("builtin.{key}"))
            .chain(users.iter().map(|(key, _)| format!("user.{key}")))
            .collect::<Vec<_>>();
        let legacy = registry
            .keys()
            .map(|key| (key.clone(), format!("builtin.{key}")));
        let recognized = RecognizedRows::new(canonical, legacy).map_err(|error| {
            source_refusal(crate::native_store::Error {
                role: Some(Role::UserModels),
                reason: crate::native_store::Reason::Document(error.reason),
            })
        })?;
        let settings =
            Settings::from_documents(recipe.clone(), profiles.clone(), users.clone(), recognized)
                .map_err(|error| {
                source_refusal(crate::native_store::Error {
                    role: error.role.or(Some(Role::ExecutionProfiles)),
                    reason: crate::native_store::Reason::Document(error.reason),
                })
            })?;
        let (state, policies) = policies(&source);
        documents.push(DocumentReport {
            file: "juiced/state.json",
            state,
            bytes: None,
            sha256: None,
            imported: state == DocumentState::Present,
            reason: match state {
                DocumentState::Present => "Only saved model policies are read, as policy suggestions.",
                DocumentState::Absent => "No native Juiced state store; policies are suggested from pinned options.",
                _ => "The native Juiced state store is unreadable; policy suggestions use pinned options only.",
            },
        });
        let (alias_report, aliases) = aliases(&source);
        documents.push(alias_report);
        let index = weights_root.as_deref().map(cache_index).unwrap_or_default();
        let mut ids: BTreeMap<String, Origin> = BTreeMap::new();
        for (key, _) in users.iter() {
            ids.insert(format!("user.{key}"), Origin::User);
        }
        let mut referenced = |id: &str| {
            let qualified = if registry.contains_key(id) && !id.starts_with("user.") {
                format!("builtin.{id}")
            } else {
                id.to_owned()
            };
            let origin = if let Some(bare) = qualified.strip_prefix("builtin.") {
                if registry.contains_key(bare) {
                    Origin::Builtin
                } else {
                    Origin::Unrecognized
                }
            } else if qualified.starts_with("user.") {
                if ids.contains_key(&qualified) {
                    Origin::User
                } else {
                    Origin::Unrecognized
                }
            } else if qualified.starts_with("extra.") {
                Origin::Extra
            } else {
                return;
            };
            ids.entry(qualified).or_insert(origin);
        };
        for (key, _) in recipe.iter().chain(profiles.iter()) {
            referenced(key);
        }
        for name in index.keys().chain(policies.keys()) {
            if registry.contains_key(name) || name.starts_with("user.") {
                referenced(name);
            }
        }
        let truncated = ids.len() > MAX_CANDIDATES;
        let mut candidates = Vec::with_capacity(ids.len().min(MAX_CANDIDATES));
        for (native_id, origin) in ids.into_iter().take(MAX_CANDIDATES) {
            let (public_name, definition) = match origin {
                Origin::Builtin => {
                    let bare = native_id.trim_start_matches("builtin.").to_owned();
                    let row = registry.get(&bare).and_then(|row| definition(row));
                    (bare, row)
                }
                Origin::User => {
                    let bare = &native_id["user.".len()..];
                    (native_id.clone(), users.row(bare).and_then(definition))
                }
                Origin::Extra | Origin::Unrecognized => (native_id.clone(), None),
            };
            let saved = matches!(origin, Origin::Builtin | Origin::User)
                .then(|| settings.saved_options(&native_id).ok())
                .flatten();
            let selected_profile = profiles
                .row(&native_id)
                .and_then(|row| {
                    serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(row.get()).ok()
                })
                .and_then(|row| {
                    row.get("selected")
                        .and_then(|value| serde_json::from_str::<String>(value.get()).ok())
                })
                .filter(|selected| selected.len() <= 160)
                .map(|selected| display_key(&selected));
            let pinned = saved
                .as_ref()
                .and_then(|saved| saved.get("pinned"))
                .is_some_and(|value| value.get() == "true");
            let previous_policy = policies
                .get(&public_name)
                .or_else(|| policies.get(&native_id))
                .map(|policy| (*policy, "native-juiced-state"))
                .or_else(|| pinned.then_some(("hot", "pinned")));
            let downloaded_index = index
                .get(&public_name)
                .or_else(|| index.get(native_id.trim_start_matches("user.")))
                .cloned();
            candidates.push(Candidate {
                native_id,
                public_name,
                origin,
                definition,
                selected_profile,
                saved,
                previous_policy,
                downloaded_index,
            });
        }
        cohort.verify().map_err(source_refusal)?;
        Ok(Self {
            source,
            weights_root,
            documents,
            config,
            aliases,
            candidates,
            truncated,
            roots: canonical_roots(roots),
            _cohort: cohort,
        })
    }

    pub fn find_weights(&self, candidate: &Candidate) -> Result<Weights, &'static str> {
        let definition = candidate
            .definition
            .as_ref()
            .ok_or("native_definition_missing")?;
        let checkpoint = definition
            .checkpoint
            .as_deref()
            .filter(|text| !text.is_empty())
            .ok_or("native_checkpoint_missing")?;
        match definition.source.as_deref() {
            Some("local_path") => {
                let path = Path::new(checkpoint);
                if !bounded_absolute(path) {
                    return Err("weights_outside_import_roots");
                }
                let (path, bytes) =
                    confine_file(path, &self.roots).ok_or("weights_outside_import_roots")?;
                return Ok(Weights {
                    files: vec![(path.clone(), bytes)],
                    path,
                    bytes,
                    selection: "local-path",
                    snapshot: None,
                });
            }
            Some("local_upload") => {
                let weights = self.weights_root.as_ref().ok_or("weights_root_missing")?;
                let relative = Path::new(checkpoint);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
                {
                    return Err("weights_missing");
                }
                let target = weights.join(relative);
                if let Some((path, bytes)) = confine_file(&target, &self.roots) {
                    return Ok(Weights {
                        files: vec![(path.clone(), bytes)],
                        path,
                        bytes,
                        selection: "local-upload",
                        snapshot: None,
                    });
                }
                let files = list_files(&target, &self.roots);
                return select(&files, None, "").ok_or("weights_missing");
            }
            Some(source) if source != "huggingface" && source != "hf" => {
                return Err("weights_registry_unsupported");
            }
            _ => {}
        }
        let weights = self.weights_root.as_ref().ok_or("weights_root_missing")?;
        let (repo, variant) = match checkpoint.split_once(':') {
            Some((repo, variant)) => (repo, Some(variant)),
            None => (checkpoint, None),
        };
        let directory = cache_directory(repo).ok_or("native_checkpoint_invalid")?;
        let repo_directory = weights.join(directory);
        let snapshot = candidate
            .downloaded_index
            .as_ref()
            .filter(|index| index.directory == repo_directory)
            .and_then(|index| index.snapshot.clone())
            .or_else(|| main_ref(&repo_directory))
            .or_else(|| only_snapshot(&repo_directory))
            .filter(|snapshot| valid_revision(snapshot))
            .ok_or("weights_missing")?;
        let snapshot_directory = repo_directory.join("snapshots").join(&snapshot);
        let files = list_files(&snapshot_directory, &self.roots);
        let repo_name = repo.rsplit('/').next().unwrap_or(repo);
        let mut selected = select(&files, variant, repo_name).ok_or("weights_missing")?;
        selected.snapshot = Some(snapshot);
        Ok(selected)
    }

    pub fn find_companion(&self, checkpoint: &str) -> bool {
        let Some(weights) = &self.weights_root else {
            return false;
        };
        let (repo, variant) = match checkpoint.split_once(':') {
            Some((repo, variant)) => (repo, Some(variant)),
            None => (checkpoint, None),
        };
        let Some(directory) = cache_directory(repo) else {
            return false;
        };
        let repo_directory = weights.join(directory);
        let Some(snapshot) = main_ref(&repo_directory).or_else(|| only_snapshot(&repo_directory))
        else {
            return false;
        };
        let files = list_files(
            &repo_directory.join("snapshots").join(snapshot),
            &self.roots,
        );
        variant.is_some_and(|variant| {
            files.iter().any(|(name, ..)| {
                name.eq_ignore_ascii_case(variant)
                    || name
                        .to_ascii_lowercase()
                        .ends_with(&format!("/{}", variant.to_ascii_lowercase()))
            })
        })
    }

    pub fn verify(&self) -> Result<(), Refusal> {
        self._cohort.verify().map_err(source_refusal)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Weights {
    pub path: PathBuf,
    pub bytes: u64,
    #[serde(skip)]
    pub files: Vec<(PathBuf, u64)>,
    pub selection: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

fn cache_directory(repo: &str) -> Option<String> {
    let (owner, name) = repo.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    (valid(owner) && valid(name)).then(|| format!("models--{owner}--{name}"))
}

fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 64
        && revision.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn main_ref(repo: &Path) -> Option<String> {
    let bytes = observe_bounded(&repo.join("refs"), "main", 128).ok()??;
    let text = String::from_utf8(bytes).ok()?;
    let text = text.trim();
    valid_revision(text).then(|| text.to_owned())
}

fn only_snapshot(repo: &Path) -> Option<String> {
    let mut entries = std::fs::read_dir(repo.join("snapshots"))
        .ok()?
        .flatten()
        .take(2);
    let first = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    first.file_name().into_string().ok()
}

fn list_files(root: &Path, roots: &[PathBuf]) -> Vec<(String, PathBuf, u64)> {
    let mut files = Vec::new();
    let mut pending = vec![(root.to_owned(), String::new(), 0usize)];
    while let Some((directory, prefix, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if files.len() >= MAX_REPO_FILES {
                break;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() && depth < MAX_REPO_DEPTH {
                pending.push((path, relative, depth + 1));
            } else if metadata.is_file()
                && let Some((canonical, bytes)) = confine_file(&path, roots)
            {
                files.push((relative, canonical, bytes));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn is_weight(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".gguf", ".bin", ".safetensors", ".onnx"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn shard_group(name: &str) -> Option<&str> {
    let lower = name.strip_suffix(".gguf")?;
    let (stem, tail) = lower.rsplit_once("-of-")?;
    if !tail.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (group, index) = stem.rsplit_once('-')?;
    index
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(group)
}

/// Mirrors the native CHECKPOINT:VARIANT matching for files already in a snapshot: an exact
/// file name, a quantization suffix, a folder, or (without a variant) the first non-projector
/// weight file, preferring files named after the repository.
fn select(
    files: &[(String, PathBuf, u64)],
    variant: Option<&str>,
    repo_name: &str,
) -> Option<Weights> {
    let weights: Vec<&(String, PathBuf, u64)> = files
        .iter()
        .filter(|(name, ..)| is_weight(name) && !name.to_ascii_lowercase().contains("mmproj"))
        .collect();
    let base = |name: &str| name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    let (matched, selection): (Vec<&(String, PathBuf, u64)>, &'static str) = match variant
        .filter(|variant| !variant.is_empty() && *variant != "*")
    {
        Some(variant) => {
            let want = variant.to_ascii_lowercase();
            let exact: Vec<_> = weights
                .iter()
                .copied()
                .filter(|(name, ..)| name.to_ascii_lowercase() == want || base(name) == want)
                .collect();
            if !exact.is_empty() {
                (exact, "exact-file")
            } else {
                let folder: Vec<_> = weights
                    .iter()
                    .copied()
                    .filter(|(name, ..)| name.to_ascii_lowercase().starts_with(&format!("{want}/")))
                    .collect();
                if !folder.is_empty() {
                    (folder, "variant-folder")
                } else {
                    let suffix = format!("{want}.gguf");
                    let matched: Vec<_> = weights
                        .iter()
                        .copied()
                        .filter(|(name, ..)| {
                            let name = base(name);
                            name.ends_with(&suffix)
                                || shard_group(&name).is_some_and(|group| group.ends_with(&want))
                                || name.contains(&format!("-{want}-"))
                        })
                        .collect();
                    (matched, "variant")
                }
            }
        }
        None => {
            let repo = repo_name.to_ascii_lowercase();
            let repo = repo.strip_suffix("-gguf").unwrap_or(&repo).to_owned();
            let preferred: Vec<_> = weights
                .iter()
                .copied()
                .filter(|(name, ..)| !repo.is_empty() && base(name).starts_with(&repo))
                .collect();
            let chosen = preferred.first().or(weights.first()).copied()?;
            let group = shard_group(&base(&chosen.0)).map(str::to_owned);
            let members: Vec<_> = match group {
                Some(group) => weights
                    .iter()
                    .copied()
                    .filter(|(name, ..)| shard_group(&base(name)) == Some(group.as_str()))
                    .collect(),
                None => vec![chosen],
            };
            (
                members,
                if preferred.is_empty() {
                    "first-weight"
                } else {
                    "repository-name"
                },
            )
        }
    };
    let primary = matched.first()?;
    let bytes = matched.iter().map(|(_, _, bytes)| *bytes).sum();
    Some(Weights {
        path: primary.1.clone(),
        bytes,
        files: matched
            .iter()
            .map(|(_, path, bytes)| (path.clone(), *bytes))
            .collect(),
        selection,
        snapshot: None,
    })
}

#[cfg(test)]
mod tests;
