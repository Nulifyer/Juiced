use crate::{
    native_settings::{Document, RecognizedRows, Role, Settings},
    native_store::{SourceCohort, descriptor::Presence, generations::PreparedBinding},
    profiles::{DeclaredContext, LaunchOptions},
};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, sync::OnceLock};

const REGISTRY: &str = include_str!("../../../../src/cpp/resources/server_models.json");
const ARCHITECTURES: &str =
    include_str!("../../../../src/cpp/resources/architecture_defaults.json");
const FLAGS: [&str; 2] = ["--jinja", "--metrics"];
const ROLES: [Role; 6] = [
    Role::Config,
    Role::Jobs,
    Role::RecipeOptions,
    Role::UserModels,
    Role::ExecutionProfiles,
    Role::NativeHelpers,
];

type Object<'a> = BTreeMap<String, &'a RawValue>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    Document(Role),
    ModelId,
    RegistrationMissing,
    UnsupportedRegistration,
    UnsupportedRecipe,
    AlternativeProfile,
    GlobalConfig,
    NativeWorkUnreconciled,
    CompanionInputs,
    UnsupportedOptions,
    AutomaticContext,
    InvalidContext,
    BackendUnresolved,
    UnsupportedBackend,
    CustomArguments,
    UnsupportedPolicy,
    ArchitectureUnresolved,
    ArchitectureDefaults,
    PhysicalSlots,
}
impl Blocker {
    pub fn code(self) -> &'static str {
        match self {
            Self::Document(_) => "model_settings_recovery_required",
            Self::ModelId | Self::RegistrationMissing => "native_registration_unbound",
            Self::AutomaticContext => "native_context_automatic",
            Self::InvalidContext => "native_context_invalid",
            Self::BackendUnresolved => "native_backend_unresolved",
            Self::UnsupportedBackend => "native_backend_unsupported",
            Self::ArchitectureUnresolved => "native_architecture_unresolved",
            Self::PhysicalSlots => "native_slots_unsupported",
            _ => "native_execution_unsupported",
        }
    }
}
impl fmt::Display for Blocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("The retained native settings are outside the selected registered llama.cpp CPU execution subset.")
    }
}
impl std::error::Error for Blocker {}

pub struct NativeLaunchView {
    native_model_id: String,
    options: LaunchOptions,
    extra_arguments: Vec<String>,
    semantic_digest: String,
}
impl fmt::Debug for NativeLaunchView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeLaunchView { .. }")
    }
}
impl NativeLaunchView {
    pub fn native_model_id(&self) -> &str {
        &self.native_model_id
    }
    pub fn selected_profile(&self) -> &'static str {
        "registered"
    }
    pub fn options(&self) -> &LaunchOptions {
        &self.options
    }
    pub fn extra_arguments(&self) -> &[String] {
        &self.extra_arguments
    }
    pub fn semantic_digest(&self) -> &str {
        &self.semantic_digest
    }
}

pub fn resolve_registered_llama(
    binding: &PreparedBinding,
    native_model_id: &str,
    metadata: &DeclaredContext,
    physical_slots: u32,
) -> Result<NativeLaunchView, Blocker> {
    ResolutionContext::from_binding(binding)?.resolve(native_model_id, metadata, physical_slots)
}

pub struct ResolutionContext {
    documents: Documents,
    settings: Settings,
    execution_blocker: Option<Blocker>,
}
impl fmt::Debug for ResolutionContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolutionContext { .. }")
    }
}
impl ResolutionContext {
    pub fn from_binding(binding: &PreparedBinding) -> Result<Self, Blocker> {
        let documents = ROLES.map(|role| {
            Document::parse(role, current_bytes(binding, role)?)
                .map_err(|_| Blocker::Document(role))
        });
        Self::from_parsed(documents)
    }
    pub fn rebased(&self, binding: &PreparedBinding) -> Result<Self, Blocker> {
        self.rebased_bytes(ROLES.map(|role| current_bytes(binding, role)))
    }
    fn rebased_bytes(
        &self,
        bytes: [Result<Option<std::sync::Arc<Vec<u8>>>, Blocker>; 6],
    ) -> Result<Self, Blocker> {
        let mut bytes = bytes.into_iter();
        Self::from_parsed(ROLES.map(|role| {
            self.documents
                .get(role)?
                .rebase_current(bytes.next().expect("six fixed role entries")?)
                .map_err(|_| Blocker::Document(role))
        }))
    }
    pub fn from_source(source: &SourceCohort) -> Result<Self, Blocker> {
        Self::from_parsed(ROLES.map(|role| {
            source
                .file(role)
                .map(|file| file.document().clone())
                .ok_or(Blocker::Document(role))
        }))
    }
    fn from_parsed(documents: [Result<Document, Blocker>; 6]) -> Result<Self, Blocker> {
        let [config, jobs, recipe, users, profiles, helpers] = documents;
        Self::new(Documents {
            config: config?,
            jobs: jobs?,
            recipe: recipe?,
            users: users?,
            profiles: profiles?,
            helpers: helpers?,
        })
    }
    fn new(documents: Documents) -> Result<Self, Blocker> {
        let execution_blocker = if !documents.config.is_empty() {
            Some(Blocker::GlobalConfig)
        } else if !documents.jobs.is_empty() || !documents.helpers.is_empty() {
            Some(Blocker::NativeWorkUnreconciled)
        } else {
            None
        };
        for key in registry_keys() {
            if let Some(row) = documents.recipe.row(key)
                && !row.get().trim_start().starts_with('{')
            {
                return Err(Blocker::Document(Role::RecipeOptions));
            }
        }
        let recognition = RecognizedRows::new(
            documents.users.iter().map(|(key, _)| format!("user.{key}")),
            [],
        )
        .map_err(|_| Blocker::Document(Role::UserModels))?;
        let settings = Settings::from_documents(
            documents.recipe.clone(),
            documents.profiles.clone(),
            documents.users.clone(),
            recognition,
        )
        .map_err(|error| Blocker::Document(error.role.unwrap_or(Role::ExecutionProfiles)))?;
        Ok(Self {
            documents,
            settings,
            execution_blocker,
        })
    }
    pub fn with_settings(&self, candidate: &Settings) -> Result<Self, Blocker> {
        let users = candidate
            .document(Role::UserModels)
            .ok_or(Blocker::Document(Role::UserModels))?;
        if !users.same_view(&self.documents.users) {
            return Err(Blocker::UnsupportedRegistration);
        }
        Self::new(Documents {
            config: self.documents.config.clone(),
            jobs: self.documents.jobs.clone(),
            recipe: candidate
                .document(Role::RecipeOptions)
                .ok_or(Blocker::Document(Role::RecipeOptions))?
                .clone(),
            users: users.clone(),
            profiles: candidate
                .document(Role::ExecutionProfiles)
                .ok_or(Blocker::Document(Role::ExecutionProfiles))?
                .clone(),
            helpers: self.documents.helpers.clone(),
        })
    }

    pub fn settings(&self, native_model_id: &str) -> Result<&Settings, Blocker> {
        let bare = model_id(native_model_id)?;
        if self.documents.users.row(bare).is_none() {
            return Err(Blocker::RegistrationMissing);
        }
        Ok(&self.settings)
    }
}

#[derive(Clone)]
struct Documents {
    config: Document,
    jobs: Document,
    recipe: Document,
    users: Document,
    profiles: Document,
    helpers: Document,
}

impl Documents {
    fn get(&self, role: Role) -> Result<&Document, Blocker> {
        match role {
            Role::Config => Ok(&self.config),
            Role::Jobs => Ok(&self.jobs),
            Role::RecipeOptions => Ok(&self.recipe),
            Role::UserModels => Ok(&self.users),
            Role::ExecutionProfiles => Ok(&self.profiles),
            Role::NativeHelpers => Ok(&self.helpers),
            Role::Aliases => Err(Blocker::Document(role)),
        }
    }
}
fn current_bytes(
    binding: &PreparedBinding,
    role: Role,
) -> Result<Option<std::sync::Arc<Vec<u8>>>, Blocker> {
    match binding.binding().current().files().get(role) {
        Some(Presence::Absent) => Ok(None),
        Some(Presence::Blob(_)) => Ok(Some(
            binding
                .file(role)
                .ok_or(Blocker::Document(role))?
                .shared_bytes(),
        )),
        None => Err(Blocker::Document(role)),
    }
}

fn object(value: &RawValue, blocker: Blocker) -> Result<Object<'_>, Blocker> {
    serde_json::from_str(value.get()).map_err(|_| blocker)
}
fn text(value: &RawValue) -> Option<String> {
    serde_json::from_str(value.get()).ok()
}
fn number(value: &RawValue) -> Option<i64> {
    serde_json::from_str(value.get()).ok()
}
fn is_null(value: &RawValue) -> bool {
    value.get() == "null"
}
fn boolean(value: &RawValue, expected: bool) -> bool {
    serde_json::from_str::<bool>(value.get()).ok() == Some(expected)
}
fn empty_text(value: &RawValue) -> bool {
    text(value).is_some_and(|text| text.is_empty())
}
fn qualified(id: &str) -> bool {
    ["user.", "extra.", "builtin."]
        .iter()
        .any(|prefix| id.starts_with(prefix))
}
fn registry_keys() -> &'static Vec<String> {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let registry: Object<'_> =
            serde_json::from_str(REGISTRY).expect("compiled native registry is valid JSON");
        registry.into_keys().filter(|key| !qualified(key)).collect()
    })
}
fn architectures() -> &'static BTreeMap<String, Box<RawValue>> {
    static OPTIONS: OnceLock<BTreeMap<String, Box<RawValue>>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        serde_json::from_str(ARCHITECTURES)
            .expect("compiled native architecture defaults are valid JSON")
    })
}

#[cfg(test)]
fn resolve_documents(
    documents: Documents,
    native_model_id: &str,
    metadata: &DeclaredContext,
    physical_slots: u32,
) -> Result<NativeLaunchView, Blocker> {
    ResolutionContext::new(documents)?.resolve(native_model_id, metadata, physical_slots)
}

fn model_id(native_model_id: &str) -> Result<&str, Blocker> {
    let bare = native_model_id
        .strip_prefix("user.")
        .filter(|bare| !bare.is_empty())
        .ok_or(Blocker::ModelId)?;
    if native_model_id.len() > 256
        || native_model_id.chars().any(char::is_control)
        || bare.starts_with("builtin.")
        || bare.starts_with("extra.")
    {
        return Err(Blocker::ModelId);
    }
    Ok(bare)
}

impl ResolutionContext {
    pub fn resolve(
        &self,
        native_model_id: &str,
        metadata: &DeclaredContext,
        physical_slots: u32,
    ) -> Result<NativeLaunchView, Blocker> {
        let bare = model_id(native_model_id)?;
        if !(1..=crate::profiles::MAX_PARALLEL_SLOTS).contains(&physical_slots) {
            return Err(Blocker::PhysicalSlots);
        }
        if let Some(blocker) = self.execution_blocker {
            return Err(blocker);
        }
        let registration = self
            .documents
            .users
            .row(bare)
            .ok_or(Blocker::RegistrationMissing)?;
        let registration = object(registration, Blocker::UnsupportedRegistration)?;
        validate_registration(&registration)?;
        let registered = match registration.get("recipe_options") {
            Some(value) => object(value, Blocker::UnsupportedOptions)?,
            None => Object::new(),
        };
        validate_options(&registered)?;
        let settings = &self.settings;
        if let Some(profile) = settings
            .document(Role::ExecutionProfiles)
            .and_then(|document| document.row(native_model_id))
        {
            let profile = object(profile, Blocker::AlternativeProfile)?;
            if let Some(selected) = profile.get("selected")
                && text(selected).as_deref() != Some("registered")
            {
                return Err(Blocker::AlternativeProfile);
            }
            for key in profile.keys() {
                if !["selected", "profiles", "settings"].contains(&key.as_str()) {
                    return Err(Blocker::UnsupportedRegistration);
                }
            }
            if let Some(profiles) = profile.get("profiles") {
                let definitions = object(profiles, Blocker::AlternativeProfile)?;
                if let Some(current) = definitions.get("registered") {
                    let current = object(current, Blocker::AlternativeProfile)?;
                    if current
                        .get("recipe")
                        .and_then(|value| text(value))
                        .as_deref()
                        != Some("llamacpp")
                    {
                        return Err(Blocker::UnsupportedRecipe);
                    }
                    if current.keys().any(|key| {
                        !["id", "recipe", "options", "saved_options"].contains(&key.as_str())
                    }) {
                        return Err(Blocker::UnsupportedRegistration);
                    }
                }
            }
        }
        let saved_options = settings
            .saved_options(native_model_id)
            .map_err(|_| Blocker::Document(Role::ExecutionProfiles))?;
        let saved: Object<'_> = saved_options
            .iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        validate_options(&saved)?;
        let mut effective = registered;
        effective.extend(saved);
        let context = match effective.get("ctx_size") {
            None => return Err(Blocker::AutomaticContext),
            Some(value) if is_null(value) => return Err(Blocker::AutomaticContext),
            Some(value) => number(value).ok_or(Blocker::InvalidContext)?,
        };
        if context == -1 {
            return Err(Blocker::AutomaticContext);
        }
        let context = u32::try_from(context)
            .ok()
            .filter(|context| *context != 0)
            .ok_or(Blocker::InvalidContext)?;
        match effective
            .get("llamacpp_backend")
            .and_then(|value| text(value))
            .as_deref()
        {
            Some("cpu") => {}
            None | Some("" | "auto") => return Err(Blocker::BackendUnresolved),
            _ => return Err(Blocker::UnsupportedBackend),
        }
        let architecture = metadata
            .architecture
            .as_deref()
            .filter(|architecture| {
                !architecture.is_empty()
                    && architecture.len() <= 256
                    && !architecture.chars().any(char::is_control)
            })
            .ok_or(Blocker::ArchitectureUnresolved)?;
        if metadata.source != "gguf:metadata" || !matches!(metadata.format_version, 2 | 3) {
            return Err(Blocker::ArchitectureUnresolved);
        }
        if let Some(defaults) = architectures().get(architecture)
            && defaults.get().trim_start().starts_with('{')
            && !object(defaults, Blocker::ArchitectureDefaults)?.is_empty()
        {
            return Err(Blocker::ArchitectureDefaults);
        }
        let options = LaunchOptions {
            context_tokens: Some(context),
            threads: None,
            gpu_layers: Some(0),
            parallel_slots: Some(physical_slots),
        };
        options.validate().map_err(|_| Blocker::InvalidContext)?;
        let identity = serde_json::to_vec(&(
            "juiced-native-registered-llama-cpu-v1",
            native_model_id,
            "registered",
            "llamacpp",
            "cpu",
            architecture,
            &options,
            FLAGS,
        ))
        .map_err(|_| Blocker::UnsupportedOptions)?;
        Ok(NativeLaunchView {
            native_model_id: native_model_id.into(),
            options,
            extra_arguments: FLAGS.into_iter().map(str::to_owned).collect(),
            semantic_digest: format!("{:x}", Sha256::digest(identity)),
        })
    }
}

fn validate_registration(registration: &Object<'_>) -> Result<(), Blocker> {
    for key in registration.keys() {
        if [
            "mmproj",
            "checkpoints",
            "components",
            "hf_load",
            "gguf_import",
            "tokenizer",
        ]
        .contains(&key.as_str())
        {
            return Err(Blocker::CompanionInputs);
        }
        if ![
            "recipe",
            "checkpoint",
            "recipe_options",
            "labels",
            "suggested",
            "size",
        ]
        .contains(&key.as_str())
        {
            return Err(Blocker::UnsupportedRegistration);
        }
    }
    if registration
        .get("recipe")
        .and_then(|value| text(value))
        .as_deref()
        != Some("llamacpp")
    {
        return Err(Blocker::UnsupportedRecipe);
    }
    if !registration
        .get("checkpoint")
        .and_then(|value| text(value))
        .is_some_and(|checkpoint| {
            !checkpoint.is_empty()
                && checkpoint.len() <= 4096
                && !checkpoint.chars().any(char::is_control)
        })
    {
        return Err(Blocker::UnsupportedRegistration);
    }
    if let Some(labels) = registration.get("labels")
        && serde_json::from_str::<Vec<String>>(labels.get())
            .ok()
            .as_deref()
            != Some(&["chat".into()])
    {
        return Err(Blocker::UnsupportedRegistration);
    }
    if let Some(suggested) = registration.get("suggested")
        && serde_json::from_str::<bool>(suggested.get()).is_err()
    {
        return Err(Blocker::UnsupportedRegistration);
    }
    if let Some(size) = registration.get("size")
        && serde_json::from_str::<f64>(size.get())
            .ok()
            .is_none_or(|size| !size.is_finite() || size < 0.0)
    {
        return Err(Blocker::UnsupportedRegistration);
    }
    Ok(())
}
fn validate_options(options: &Object<'_>) -> Result<(), Blocker> {
    for (key, value) in options {
        match key.as_str() {
            "ctx_size" => {}
            "llamacpp_backend" => {
                if !is_null(value) && text(value).is_none() {
                    return Err(Blocker::BackendUnresolved);
                }
            }
            "llamacpp_args" | "llamacpp_device" => {
                if !empty_text(value) {
                    return Err(Blocker::CustomArguments);
                }
            }
            "merge_args" => {
                if !boolean(value, true) {
                    return Err(Blocker::CustomArguments);
                }
            }
            "pinned" => {
                if !boolean(value, false) {
                    return Err(Blocker::UnsupportedPolicy);
                }
            }
            "auto_evict" | "auto_update" => {
                if !is_null(value) && !boolean(value, false) {
                    return Err(Blocker::UnsupportedPolicy);
                }
            }
            "evict_idle_timeout" => {
                if number(value) != Some(300) {
                    return Err(Blocker::UnsupportedPolicy);
                }
            }
            "downsize_idle_timeout" => {
                if number(value) != Some(60) {
                    return Err(Blocker::UnsupportedPolicy);
                }
            }
            "evict_weight_factor" => {
                if serde_json::from_str::<f64>(value.get()).ok() != Some(1.0) {
                    return Err(Blocker::UnsupportedPolicy);
                }
            }
            "llamacpp_hybrid_memory_mib" => {
                if number(value) != Some(2048) {
                    return Err(Blocker::UnsupportedOptions);
                }
            }
            "llamacpp_hybrid_min_prompt" => {
                if number(value) != Some(1024) {
                    return Err(Blocker::UnsupportedOptions);
                }
            }
            _ => return Err(Blocker::UnsupportedOptions),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_settings::SavedOptions;
    use serde_json::{Value, json};
    use std::error::Error as _;

    const MODEL: &str = "user.Selected";
    fn metadata() -> DeclaredContext {
        DeclaredContext {
            source: "gguf:metadata",
            format_version: 3,
            architecture: Some("llama".into()),
            training_context_tokens: Some(8192),
        }
    }
    fn document(role: Role, bytes: &str) -> Document {
        Document::parse_bytes(role, Some(bytes.as_bytes())).unwrap()
    }
    fn fixture() -> Documents {
        Documents {
            config: Document::parse(Role::Config, None).unwrap(),
            jobs: document(Role::Jobs, "{}"),
            recipe: document(Role::RecipeOptions, "{}"),
            users: document(
                Role::UserModels,
                r#"{"Selected":{"recipe":"llamacpp","checkpoint":"fixture/repo:Q4_K_M","labels":["chat"],"recipe_options":{"ctx_size":1536,"llamacpp_backend":"cpu"}}}"#,
            ),
            profiles: Document::parse(Role::ExecutionProfiles, None).unwrap(),
            helpers: Document::parse(Role::NativeHelpers, None).unwrap(),
        }
    }
    fn resolve(documents: &Documents) -> Result<NativeLaunchView, Blocker> {
        resolve_documents(documents.clone(), MODEL, &metadata(), 1)
    }
    fn saved(documents: &mut Documents, value: Value) {
        documents.recipe = document(
            Role::RecipeOptions,
            &serde_json::to_string(&json!({MODEL:value})).unwrap(),
        );
    }

    #[test]
    fn immutable_context_reuses_parsed_rows_and_settings_for_multiple_resolutions() {
        let mut documents = fixture();
        documents.recipe = document(
            Role::RecipeOptions,
            r#"{"opaque":{"number":123456789012345678901234567890}}"#,
        );
        let raw = documents.recipe.row("opaque").unwrap() as *const RawValue;
        let original_bytes = documents.recipe.original_bytes().unwrap().clone();
        let context = ResolutionContext::new(documents.clone()).unwrap();
        assert!(std::ptr::eq(
            context.settings(MODEL).unwrap(),
            context.settings(MODEL).unwrap()
        ));
        assert_eq!(
            context
                .settings(MODEL)
                .unwrap()
                .document(Role::RecipeOptions)
                .unwrap()
                .row("opaque")
                .unwrap() as *const RawValue,
            raw
        );
        let digest = context
            .resolve(MODEL, &metadata(), 1)
            .unwrap()
            .semantic_digest()
            .to_owned();
        for _ in 0..3 {
            assert_eq!(
                context
                    .resolve(MODEL, &metadata(), 1)
                    .unwrap()
                    .semantic_digest(),
                digest
            );
        }
        saved(&mut documents, json!({"ctx_size":4096}));
        assert_ne!(
            ResolutionContext::new(documents)
                .unwrap()
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .semantic_digest(),
            digest
        );
        assert_eq!(
            context
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(1536)
        );
        assert_eq!(
            context
                .settings(MODEL)
                .unwrap()
                .document(Role::RecipeOptions)
                .unwrap()
                .original_bytes()
                .unwrap()
                .as_ref(),
            original_bytes.as_ref()
        );
        assert!(context.settings("user.Unregistered").is_err());
        let mut blocked = fixture();
        blocked.config = document(Role::Config, r#"{"retained":"global settings"}"#);
        let context = ResolutionContext::new(blocked).unwrap();
        context.settings(MODEL).unwrap();
        assert_eq!(
            context.resolve(MODEL, &metadata(), 1).unwrap_err(),
            Blocker::GlobalConfig
        );
    }

    #[test]
    fn option_candidate_context_uses_current_rows_preserves_unknowns_and_clears_to_registered_defaults()
     {
        let large = "x".repeat(1024 * 1024);
        let mut documents = fixture();
        documents.recipe = document(
            Role::RecipeOptions,
            &format!(
                r#"{{"user.Selected":{{"ctx_size":2048}},"opaque":{{"large":"{large}","n":123456789012345678901234567890}}}}"#
            ),
        );
        let original = documents.recipe.original_bytes().unwrap().clone();
        let opaque = documents.recipe.row("opaque").unwrap() as *const RawValue;
        let context = ResolutionContext::new(documents).unwrap();
        let digest = context
            .resolve(MODEL, &metadata(), 1)
            .unwrap()
            .semantic_digest()
            .to_owned();
        let changed = context
            .settings(MODEL)
            .unwrap()
            .merge_options(
                MODEL,
                &SavedOptions::parse(br#"{"ctx_size":4096}"#).unwrap(),
            )
            .unwrap();
        let updated = context.with_settings(&changed).unwrap();
        assert_eq!(
            updated
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(4096)
        );
        assert_ne!(
            updated
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .semantic_digest(),
            digest
        );
        let cleared = updated
            .settings(MODEL)
            .unwrap()
            .merge_options(
                MODEL,
                &SavedOptions::parse(br#"{"ctx_size":null}"#).unwrap(),
            )
            .unwrap();
        let fallback = updated.with_settings(&cleared).unwrap();
        assert_eq!(
            fallback
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(1536)
        );
        assert_eq!(
            context
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(2048)
        );
        assert_eq!(
            context
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .semantic_digest(),
            digest
        );
        for snapshot in [&context, &updated, &fallback] {
            let document = snapshot
                .settings(MODEL)
                .unwrap()
                .document(Role::RecipeOptions)
                .unwrap();
            assert_eq!(document.row("opaque").unwrap() as *const RawValue, opaque);
            assert!(std::sync::Arc::ptr_eq(
                document.original_bytes().unwrap(),
                &original
            ));
            assert_eq!(
                document.original_bytes().unwrap().as_ref(),
                original.as_ref()
            );
        }
        assert_eq!(context.documents.recipe.to_bytes().unwrap(), *original);
    }

    #[test]
    fn candidate_profile_selection_is_preflighted_from_current_rows_and_user_registration_is_fixed()
    {
        let context = ResolutionContext::new(fixture()).unwrap();
        assert!(context.documents.profiles.is_absent());
        let effective =
            SavedOptions::parse(br#"{"ctx_size":1536,"llamacpp_backend":"cpu","pinned":false}"#)
                .unwrap();
        let selection=crate::native_settings::ProfileSelection::parse(br#"{"id":"alternate","recipe":"llamacpp","options":{"ctx_size":4096,"llamacpp_backend":"cpu"}}"#).unwrap();
        let changed = context
            .settings(MODEL)
            .unwrap()
            .select_profile(
                MODEL,
                crate::native_settings::SelectionContext {
                    current_recipe: "llamacpp",
                    effective_options: &effective,
                },
                &selection,
            )
            .unwrap();
        let updated = context.with_settings(&changed).unwrap();
        assert!(!updated.documents.profiles.is_absent());
        assert!(updated.documents.profiles.original_bytes().is_none());
        assert_eq!(
            updated.resolve(MODEL, &metadata(), 1).unwrap_err(),
            Blocker::AlternativeProfile
        );
        context.resolve(MODEL, &metadata(), 1).unwrap();
        assert!(context.documents.profiles.is_absent());
        let users = document(
            Role::UserModels,
            std::str::from_utf8(&context.documents.users.to_bytes().unwrap()).unwrap(),
        );
        let unrelated = Settings::from_documents(
            context.documents.recipe.clone(),
            context.documents.profiles.clone(),
            users,
            RecognizedRows::new([MODEL.into()], []).unwrap(),
        )
        .unwrap();
        assert_eq!(
            context.with_settings(&unrelated).unwrap_err(),
            Blocker::UnsupportedRegistration
        );
        let mut blocked = fixture();
        blocked.config = document(Role::Config, r#"{"retained":true}"#);
        let context = ResolutionContext::new(blocked).unwrap();
        let changed = context
            .settings(MODEL)
            .unwrap()
            .merge_options(
                MODEL,
                &SavedOptions::parse(br#"{"ctx_size":4096}"#).unwrap(),
            )
            .unwrap();
        assert_eq!(
            context
                .with_settings(&changed)
                .unwrap()
                .resolve(MODEL, &metadata(), 1)
                .unwrap_err(),
            Blocker::GlobalConfig
        );
    }

    fn commit_buffers(
        context: &ResolutionContext,
    ) -> [Result<Option<std::sync::Arc<Vec<u8>>>, Blocker>; 6] {
        ROLES.map(|role| {
            let document = context.documents.get(role)?;
            Ok(if document.is_absent() {
                None
            } else {
                Some(std::sync::Arc::new(document.to_bytes().unwrap()))
            })
        })
    }

    #[test]
    fn sequential_edit_rebases_follow_committed_bytes_without_changing_semantics_or_old_snapshots()
    {
        let mut documents = fixture();
        documents.recipe = document(
            Role::RecipeOptions,
            &format!(
                r#"{{"user.Selected":{{"ctx_size":2048}},"opaque":{{"large":"{}","n":1.23450000000000000000e+9999}}}}"#,
                "x".repeat(1024 * 1024)
            ),
        );
        let original_bytes = documents.recipe.original_bytes().unwrap().clone();
        let opaque = documents.recipe.row("opaque").unwrap() as *const RawValue;
        let original = ResolutionContext::new(documents).unwrap();
        let first = original
            .settings(MODEL)
            .unwrap()
            .merge_options(
                MODEL,
                &SavedOptions::parse(br#"{"ctx_size":4096}"#).unwrap(),
            )
            .unwrap();
        let first = original.with_settings(&first).unwrap();
        let first_digest = first
            .resolve(MODEL, &metadata(), 1)
            .unwrap()
            .semantic_digest()
            .to_owned();
        let committed = commit_buffers(&first);
        let first_bytes = committed[2].as_ref().unwrap().as_ref().unwrap().clone();
        let active = first.rebased_bytes(committed).unwrap();
        assert_eq!(
            active
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .semantic_digest(),
            first_digest
        );
        assert!(std::sync::Arc::ptr_eq(
            active.documents.recipe.original_bytes().unwrap(),
            &first_bytes
        ));
        let second = active
            .settings(MODEL)
            .unwrap()
            .merge_options(
                MODEL,
                &SavedOptions::parse(br#"{"ctx_size":null}"#).unwrap(),
            )
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(
            second
                .document(Role::RecipeOptions)
                .unwrap()
                .original_bytes()
                .unwrap(),
            &first_bytes
        ));
        let second = active.with_settings(&second).unwrap();
        assert_eq!(
            second
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(1536)
        );
        let second_digest = second
            .resolve(MODEL, &metadata(), 1)
            .unwrap()
            .semantic_digest()
            .to_owned();
        assert_ne!(first_digest, second_digest);
        let committed = commit_buffers(&second);
        let second_bytes = committed[2].as_ref().unwrap().as_ref().unwrap().clone();
        let final_view = second.rebased_bytes(committed).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            final_view.documents.recipe.original_bytes().unwrap(),
            &second_bytes
        ));
        assert_eq!(
            final_view
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .semantic_digest(),
            second_digest
        );
        for view in [&original, &first, &active, &second, &final_view] {
            assert_eq!(
                view.documents.recipe.row("opaque").unwrap() as *const RawValue,
                opaque
            );
        }
        assert_eq!(
            original.documents.recipe.to_bytes().unwrap(),
            *original_bytes
        );
        assert_eq!(
            original
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(2048)
        );
        assert_eq!(
            active
                .resolve(MODEL, &metadata(), 1)
                .unwrap()
                .options()
                .context_tokens,
            Some(4096)
        );
        let mut mismatched = commit_buffers(&final_view);
        mismatched[2] = Ok(Some(original_bytes));
        assert_eq!(
            final_view.rebased_bytes(mismatched).unwrap_err(),
            Blocker::Document(Role::RecipeOptions)
        );
        let mut absent = commit_buffers(&final_view);
        absent[2] = Ok(None);
        assert_eq!(
            final_view.rebased_bytes(absent).unwrap_err(),
            Blocker::Document(Role::RecipeOptions)
        );
    }

    #[test]
    fn selected_positive_cpu_view_derives_only_supported_values_without_launching_or_changing_bytes()
     {
        let mut documents = fixture();
        saved(
            &mut documents,
            json!({"ctx_size":2048,"pinned":false,"merge_args":true}),
        );
        let before = documents.recipe.to_bytes().unwrap();
        let result = resolve(&documents).unwrap();
        assert_eq!(result.native_model_id(), MODEL);
        assert_eq!(result.selected_profile(), "registered");
        assert_eq!(
            result.options(),
            &LaunchOptions {
                context_tokens: Some(2048),
                threads: None,
                gpu_layers: Some(0),
                parallel_slots: Some(1)
            }
        );
        assert_eq!(result.extra_arguments(), ["--jinja", "--metrics"]);
        assert_eq!(documents.recipe.to_bytes().unwrap(), before);
        assert_eq!(result.semantic_digest().len(), 64);
    }

    #[test]
    fn registered_defaults_are_under_saved_authority_and_null_patch_restores_the_registered_default()
     {
        let mut documents = fixture();
        assert_eq!(
            resolve(&documents).unwrap().options().context_tokens,
            Some(1536)
        );
        documents.profiles = document(
            Role::ExecutionProfiles,
            r#"{"user.Selected":{"selected":"registered","profiles":{"registered":{"recipe":"llamacpp","options":{"ctx_size":9999}}},"settings":{"version":1,"saved_options":{"ctx_size":4096,"llamacpp_backend":"cpu"}}}}"#,
        );
        saved(&mut documents, json!({"ctx_size":8192}));
        assert_eq!(
            resolve(&documents).unwrap().options().context_tokens,
            Some(4096)
        );
        let owner = Settings::from_documents(
            documents.recipe.clone(),
            documents.profiles.clone(),
            documents.users.clone(),
            RecognizedRows::new([MODEL.into()], []).unwrap(),
        )
        .unwrap();
        let edited = owner
            .merge_options(
                MODEL,
                &SavedOptions::parse(br#"{"ctx_size":null}"#).unwrap(),
            )
            .unwrap();
        documents.profiles = edited.document(Role::ExecutionProfiles).unwrap().clone();
        assert_eq!(
            resolve(&documents).unwrap().options().context_tokens,
            Some(1536)
        );
        assert_eq!(
            documents.recipe.row(MODEL).unwrap().get(),
            r#"{"ctx_size":8192}"#
        );
    }

    #[test]
    fn automatic_invalid_and_fractional_contexts_are_never_coerced_or_inferred_from_training_metadata()
     {
        for (context, expected) in [
            (json!(-1), Blocker::AutomaticContext),
            (Value::Null, Blocker::AutomaticContext),
            (json!(0), Blocker::InvalidContext),
            (json!(-2), Blocker::InvalidContext),
            (json!(1.5), Blocker::InvalidContext),
            (json!("auto"), Blocker::InvalidContext),
            (json!(i32::MAX as u64 + 1), Blocker::InvalidContext),
        ] {
            let mut documents = fixture();
            saved(&mut documents, json!({"ctx_size":context}));
            let before = documents.recipe.to_bytes().unwrap();
            assert_eq!(resolve(&documents).unwrap_err(), expected);
            assert_eq!(documents.recipe.to_bytes().unwrap(), before);
        }
        let mut documents = fixture();
        documents.users = document(
            Role::UserModels,
            r#"{"Selected":{"recipe":"llamacpp","checkpoint":"fixture","recipe_options":{"llamacpp_backend":"cpu"}}}"#,
        );
        assert_eq!(resolve(&documents).unwrap_err(), Blocker::AutomaticContext);
    }

    #[test]
    fn backend_device_custom_arguments_and_physical_slots_need_the_selected_contract() {
        for backend in [Value::Null, json!(""), json!("auto")] {
            let mut documents = fixture();
            saved(&mut documents, json!({"llamacpp_backend":backend}));
            assert_eq!(resolve(&documents).unwrap_err(), Blocker::BackendUnresolved);
        }
        for backend in ["system", "cuda", "hybrid-rocm", "rocm", "vulkan"] {
            let mut documents = fixture();
            saved(&mut documents, json!({"llamacpp_backend":backend}));
            assert_eq!(
                resolve(&documents).unwrap_err(),
                Blocker::UnsupportedBackend
            );
        }
        for patch in [
            json!({"llamacpp_device":"CPU"}),
            json!({"llamacpp_args":"--threads 2"}),
            json!({"llamacpp_args":"--parallel 1"}),
            json!({"merge_args":false}),
        ] {
            let mut documents = fixture();
            saved(&mut documents, patch);
            assert_eq!(resolve(&documents).unwrap_err(), Blocker::CustomArguments);
        }
        for slots in [0, 4097, u32::MAX] {
            assert_eq!(
                resolve_documents(fixture(), MODEL, &metadata(), slots).unwrap_err(),
                Blocker::PhysicalSlots
            );
        }
    }

    #[test]
    fn declared_slots_bind_identity_without_resizing_the_context_budget() {
        let one = resolve_documents(fixture(), MODEL, &metadata(), 1).unwrap();
        for slots in [2, 5, 4096] {
            let multiple = resolve_documents(fixture(), MODEL, &metadata(), slots).unwrap();
            assert_eq!(multiple.options().parallel_slots, Some(slots));
            assert_eq!(
                multiple.options().context_tokens,
                one.options().context_tokens
            );
            assert_ne!(multiple.semantic_digest(), one.semantic_digest());
            let repeated = resolve_documents(fixture(), MODEL, &metadata(), slots).unwrap();
            assert_eq!(multiple.semantic_digest(), repeated.semantic_digest());
        }
    }

    #[test]
    fn unknown_active_values_and_nondefault_policy_are_preserved_but_cannot_execute() {
        let mut documents = fixture();
        documents.recipe = document(
            Role::RecipeOptions,
            r#"{"user.Selected":{"future_option":{"large":123456789012345678901234567890}}}"#,
        );
        let before = documents.recipe.to_bytes().unwrap();
        assert_eq!(
            resolve(&documents).unwrap_err(),
            Blocker::UnsupportedOptions
        );
        assert_eq!(documents.recipe.to_bytes().unwrap(), before);
        for patch in [
            json!({"pinned":true}),
            json!({"auto_update":true}),
            json!({"auto_evict":true}),
            json!({"evict_idle_timeout":301}),
            json!({"downsize_idle_timeout":61}),
            json!({"evict_weight_factor":2}),
        ] {
            let mut documents = fixture();
            saved(&mut documents, patch);
            assert_eq!(resolve(&documents).unwrap_err(), Blocker::UnsupportedPolicy);
        }
        let mut documents = fixture();
        saved(
            &mut documents,
            json!({"auto_update":null,"auto_evict":null,"pinned":false,"evict_idle_timeout":300,"downsize_idle_timeout":60,"evict_weight_factor":1.0,"llamacpp_device":"","llamacpp_args":"","llamacpp_hybrid_memory_mib":2048,"llamacpp_hybrid_min_prompt":1024}),
        );
        resolve(&documents).unwrap();
        saved(&mut documents, json!({"threads":2}));
        assert_eq!(
            resolve(&documents).unwrap_err(),
            Blocker::UnsupportedOptions
        );
    }

    #[test]
    fn profile_alternatives_and_companions_are_not_silently_rebound_to_the_registered_cpu_worker() {
        let mut documents = fixture();
        documents.profiles = document(
            Role::ExecutionProfiles,
            r#"{"user.Selected":{"selected":"alternate","profiles":{"alternate":{"recipe":"llamacpp"}},"settings":{"version":1,"saved_options":{"ctx_size":2048,"llamacpp_backend":"cpu"}}}}"#,
        );
        assert_eq!(
            resolve(&documents).unwrap_err(),
            Blocker::AlternativeProfile
        );
        for field in [
            "mmproj",
            "checkpoints",
            "components",
            "hf_load",
            "gguf_import",
            "tokenizer",
        ] {
            let mut documents = fixture();
            let mut users: Value =
                serde_json::from_slice(&documents.users.to_bytes().unwrap()).unwrap();
            users["Selected"][field] = json!("preserved");
            documents.users = document(Role::UserModels, &users.to_string());
            assert_eq!(resolve(&documents).unwrap_err(), Blocker::CompanionInputs);
        }
        for recipe in ["vllm", "llama-xdna-hybrid", "whispercpp"] {
            let mut documents = fixture();
            let mut users: Value =
                serde_json::from_slice(&documents.users.to_bytes().unwrap()).unwrap();
            users["Selected"]["recipe"] = json!(recipe);
            documents.users = document(Role::UserModels, &users.to_string());
            assert_eq!(resolve(&documents).unwrap_err(), Blocker::UnsupportedRecipe);
        }
    }

    #[test]
    fn global_configuration_pending_native_work_and_architecture_defaults_remain_explicit_blockers()
    {
        for role in [Role::Config, Role::Jobs, Role::NativeHelpers] {
            let mut documents = fixture();
            let nonempty = document(role, r#"{"retained":{}}"#);
            match role {
                Role::Config => documents.config = nonempty,
                Role::Jobs => documents.jobs = nonempty,
                _ => documents.helpers = nonempty,
            }
            assert_eq!(
                resolve(&documents).unwrap_err(),
                if role == Role::Config {
                    Blocker::GlobalConfig
                } else {
                    Blocker::NativeWorkUnreconciled
                }
            );
        }
        for architecture in ["qwen3", "qwen35", "gemma4"] {
            let mut metadata = metadata();
            metadata.architecture = Some(architecture.into());
            assert_eq!(
                resolve_documents(fixture(), MODEL, &metadata, 1).unwrap_err(),
                Blocker::ArchitectureDefaults
            );
        }
        let mut metadata = metadata();
        metadata.architecture = None;
        assert_eq!(
            resolve_documents(fixture(), MODEL, &metadata, 1).unwrap_err(),
            Blocker::ArchitectureUnresolved
        );
    }

    #[test]
    fn exact_compiled_registry_recognizes_bare_builtin_rows_but_never_unknown_metadata_or_bindings()
    {
        let known = &registry_keys()[0];
        let mut documents = fixture();
        documents.recipe = document(Role::RecipeOptions, &json!({known:null}).to_string());
        assert_eq!(
            resolve(&documents).unwrap_err(),
            Blocker::Document(Role::RecipeOptions)
        );
        documents.recipe = document(
            Role::RecipeOptions,
            r#"{"not-a-shipped-builtin":[1,"untouched"]}"#,
        );
        resolve(&documents).unwrap();
        documents.recipe = document(Role::RecipeOptions, r#"{"builtin.Stale":null}"#);
        assert_eq!(
            resolve(&documents).unwrap_err(),
            Blocker::Document(Role::RecipeOptions)
        );
        for model in [
            "Selected",
            "builtin.Selected",
            "extra.Selected",
            "user.",
            "user.builtin.Selected",
        ] {
            assert_eq!(
                resolve_documents(fixture(), model, &metadata(), 1).unwrap_err(),
                Blocker::ModelId
            );
        }
        assert_eq!(
            resolve_documents(fixture(), "user.Unregistered", &metadata(), 1).unwrap_err(),
            Blocker::RegistrationMissing
        );
    }

    #[test]
    fn unrelated_rows_and_global_generation_identity_do_not_change_selected_model_semantics() {
        let original = fixture();
        let digest = resolve(&original).unwrap().semantic_digest().to_owned();
        let mut unrelated = fixture();
        unrelated.recipe = document(
            Role::RecipeOptions,
            r#"{"user.Other":{"ctx_size":9999},"opaque":{"number":123456789012345678901234567890}}"#,
        );
        unrelated.profiles = document(
            Role::ExecutionProfiles,
            r#"{"user.Other":{"selected":"future","profiles":{"future":{"recipe":"future-engine"}},"settings":{"version":1,"saved_options":{"opaque":"preserved"}}}}"#,
        );
        assert_eq!(resolve(&unrelated).unwrap().semantic_digest(), digest);
        saved(&mut unrelated, json!({"ctx_size":2048}));
        assert_ne!(resolve(&unrelated).unwrap().semantic_digest(), digest);
        assert!(original.profiles.is_absent());
    }

    #[test]
    fn resources_match_the_pinned_native_evidence_and_diagnostics_never_include_private_input() {
        assert_eq!(
            format!("{:x}", Sha256::digest(REGISTRY.as_bytes())),
            "63c69b91af85e10ef371b9939c305fffd0e9939bf90d7d1ba3168c8f0d54b74e"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(ARCHITECTURES.as_bytes())),
            "0b382e755ce59f30bc64b19c6b504b7577e0c2c45d44087dee6d7d8ccc9923be"
        );
        let marker = "JUICED_SYNTHETIC_PRIVATE_EXECUTION_MARKER";
        let mut documents = fixture();
        saved(&mut documents, json!({marker:marker}));
        let error = resolve(&documents).unwrap_err();
        assert!(!format!("{error:?} {error}").contains(marker));
        assert!(error.source().is_none());
        assert!(!format!("{:?}", resolve(&fixture()).unwrap()).contains(MODEL));
    }
}
