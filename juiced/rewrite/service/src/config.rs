use serde::{Deserialize, Deserializer, Serialize, de::Error};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

pub(crate) const MAXIMUM_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
pub const MAXIMUM_ALIASES: usize = 1024;

pub fn valid_model_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAliases {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub entries: std::collections::BTreeMap<String, String>,
}

impl ModelAliases {
    fn is_unset(&self) -> bool {
        self.revision == 0 && self.entries.is_empty()
    }
}

pub(crate) struct ConfigurationBinding {
    pub(crate) root: crate::storage::DirectoryLease,
    pub(crate) path: PathBuf,
}

impl ConfigurationBinding {
    pub(crate) fn claim(input: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let absolute = std::path::absolute(input)?;
        let filename = absolute
            .file_name()
            .ok_or("Configuration file name is required")?;
        let root = crate::storage::DirectoryLease::acquire(
            absolute
                .parent()
                .ok_or("Configuration directory is required")?,
        )?;
        Self::from_root(root, std::path::Path::new(filename))
    }

    pub(crate) fn from_root(
        root: crate::storage::DirectoryLease,
        filename: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let path = root.file_path(filename)?;
        Ok(Self { root, path })
    }

    pub(crate) fn filename(&self) -> &std::path::Path {
        std::path::Path::new(
            self.path
                .file_name()
                .expect("configuration binding has a filename"),
        )
    }

    pub(crate) fn read(&self) -> Result<Config, Box<dyn std::error::Error>> {
        Config::read_bound(&self.root, self.filename())
    }
}

pub struct ConfigRecoveryError {
    stage: &'static str,
    category: Option<&'static str>,
    line: Option<usize>,
    column: Option<usize>,
}

impl ConfigRecoveryError {
    const GUIDANCE: &str = "Preserve the file and restore a valid supported configuration.";

    fn decode(error: serde_json::Error) -> Self {
        let category = match error.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        };
        Self {
            stage: "decode",
            category: Some(category),
            line: (error.line() != 0).then_some(error.line()),
            column: (error.column() != 0).then_some(error.column()),
        }
    }

    fn validation() -> Self {
        Self {
            stage: "validate",
            category: None,
            line: None,
            column: None,
        }
    }

    pub fn code(&self) -> &'static str {
        "configuration_recovery_required"
    }

    pub fn stage(&self) -> &'static str {
        self.stage
    }

    pub fn category(&self) -> Option<&'static str> {
        self.category
    }

    pub fn line(&self) -> Option<usize> {
        self.line
    }

    pub fn column(&self) -> Option<usize> {
        self.column
    }
}

impl std::fmt::Display for ConfigRecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: stage={}", self.code(), self.stage)?;
        if let Some(category) = self.category {
            write!(formatter, " category={category}")?;
        }
        if let Some(line) = self.line {
            write!(formatter, " line={line}")?;
        }
        if let Some(column) = self.column {
            write!(formatter, " column={column}")?;
        }
        write!(formatter, "; {}", Self::GUIDANCE)
    }
}

impl std::fmt::Debug for ConfigRecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigRecoveryError")
            .field("code", &self.code())
            .field("stage", &self.stage)
            .field("category", &self.category)
            .field("line", &self.line)
            .field("column", &self.column)
            .field("message", &Self::GUIDANCE)
            .finish()
    }
}

impl std::error::Error for ConfigRecoveryError {}

#[derive(Debug)]
pub(crate) enum ConfigSizeError {
    TooLarge,
    Serialization(serde_json::Error),
}

impl std::fmt::Display for ConfigSizeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => {
                formatter.write_str("The canonical configuration exceeds the 16 MiB startup limit")
            }
            Self::Serialization(error) => {
                write!(formatter, "Configuration serialization failed: {error}")
            }
        }
    }
}

impl std::error::Error for ConfigSizeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge => None,
            Self::Serialization(error) => Some(error),
        }
    }
}

fn check_serialized_size(
    value: &(impl Serialize + ?Sized),
    maximum: u64,
) -> Result<(), ConfigSizeError> {
    crate::storage::check_json_size(value, maximum).map_err(|error| match error {
        crate::storage::JsonSizeError::TooLarge => ConfigSizeError::TooLarge,
        crate::storage::JsonSizeError::Serialization(error) => {
            ConfigSizeError::Serialization(error)
        }
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    #[default]
    Disabled,
    OnDemand,
    Hot,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Operation {
    #[serde(rename = "chat/completions")]
    Chat,
    #[serde(rename = "completions")]
    Completion,
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "embeddings")]
    Embeddings,
    #[serde(rename = "messages/count_tokens")]
    TokenCount,
    #[serde(rename = "audio/transcriptions")]
    Transcription,
    #[serde(rename = "audio/speech")]
    Speech,
    #[serde(rename = "images/generations")]
    ImageGeneration,
    #[serde(rename = "rerank")]
    Rerank,
}

impl Operation {
    pub fn path(self) -> &'static str {
        match self {
            Self::Chat => "chat/completions",
            Self::Completion => "completions",
            Self::Responses => "responses",
            Self::Embeddings => "embeddings",
            Self::TokenCount => "messages/count_tokens",
            Self::Transcription => "audio/transcriptions",
            Self::Speech => "audio/speech",
            Self::ImageGeneration => "images/generations",
            Self::Rerank => "rerank",
        }
    }
    pub fn single_slot_media(self) -> bool {
        matches!(
            self,
            Self::Transcription | Self::Speech | Self::ImageGeneration
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    ExternalHttp,
    LlamaCpp,
    WhisperCpp,
    StableDiffusionCpp,
    Kokoros,
    Parakeet,
    Moonshine,
    Asr4all,
    #[serde(rename = "fastflowlm")]
    FastFlowLm,
}

impl Provider {
    pub fn implementation_id(self) -> Option<&'static str> {
        match self {
            Self::ExternalHttp => None,
            Self::LlamaCpp => Some("llama-cpp"),
            Self::WhisperCpp => Some("whisper-cpp"),
            Self::StableDiffusionCpp => Some("stable-diffusion-cpp"),
            Self::Kokoros => Some("kokoros"),
            Self::Parakeet => Some("parakeet-redux"),
            Self::Moonshine => Some("moonshine-voice"),
            Self::Asr4all => Some("futo-transcribe-cpp"),
            Self::FastFlowLm => Some("fastflowlm"),
        }
    }
    pub fn from_implementation(id: &str) -> Option<Self> {
        match id {
            "llama-cpp" => Some(Self::LlamaCpp),
            "whisper-cpp" => Some(Self::WhisperCpp),
            "stable-diffusion-cpp" => Some(Self::StableDiffusionCpp),
            "kokoros" => Some(Self::Kokoros),
            "parakeet-redux" => Some(Self::Parakeet),
            "moonshine-voice" => Some(Self::Moonshine),
            "futo-transcribe-cpp" => Some(Self::Asr4all),
            "fastflowlm" => Some(Self::FastFlowLm),
            _ => None,
        }
    }
    pub fn media_operation(self) -> Option<Operation> {
        match self {
            Self::WhisperCpp | Self::Parakeet | Self::Moonshine | Self::Asr4all => {
                Some(Operation::Transcription)
            }
            Self::StableDiffusionCpp => Some(Operation::ImageGeneration),
            Self::Kokoros => Some(Operation::Speech),
            Self::ExternalHttp | Self::LlamaCpp | Self::FastFlowLm => None,
        }
    }
    pub fn readiness_path(self) -> &'static str {
        match self {
            Self::StableDiffusionCpp | Self::Kokoros => "/v1/models",
            Self::FastFlowLm => "/api/tags",
            Self::ExternalHttp
            | Self::LlamaCpp
            | Self::WhisperCpp
            | Self::Parakeet
            | Self::Moonshine
            | Self::Asr4all => "/health",
        }
    }
    pub fn accepts_companions(self) -> bool {
        matches!(
            self,
            Self::StableDiffusionCpp | Self::Kokoros | Self::Moonshine
        )
    }
    pub fn transcription_path(self) -> &'static str {
        match self {
            Self::Parakeet => "/v1/audio/transcriptions",
            _ => "/inference",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub id: String,
    pub model_file: PathBuf,
    pub execution_profile: crate::execution_profile::ExecutionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_profile: Option<crate::resource_profile::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_artifact_set: Option<std::sync::Arc<crate::artifacts::ArtifactBinding>>,
    pub resident_bytes: u64,
    pub max_inflight: u32,
    #[serde(default)]
    pub essential_headroom: u32,
    pub operations: Vec<Operation>,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_unload_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companions: Vec<CompanionFile>,
}

pub const MAXIMUM_COMPANIONS: usize = 8;

/// A companion file registered in place beside an unmanaged model file. Launch checks that it
/// still exists and binds it to the `{member:NAME}` argument with the same name.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompanionFile {
    pub name: String,
    pub role: crate::artifacts::ArtifactRole,
    pub path: PathBuf,
}

/// Records that the native configuration directory was adopted, so restarts do not repeat it.
/// `pending` lists native model IDs whose registration still waits for weights or an engine.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeAdoption {
    pub source: PathBuf,
    pub adopted_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
}

impl ModelSpec {
    pub fn resolved(&self, parent: &Path) -> Self {
        let mut model = self.clone();
        if model.model_file.is_relative() {
            model.model_file = parent.join(model.model_file);
        }
        let profile = &mut model.execution_profile;
        if (profile.command.contains('/') || profile.command.contains('\\'))
            && Path::new(&profile.command).is_relative()
        {
            profile.command = parent.join(&profile.command).to_string_lossy().into_owned();
        }
        profile.resolve_bundle_root(parent);
        model
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub cache_enabled: bool,
    #[serde(default, skip_serializing_if = "crate::sessions::Settings::is_default")]
    pub session_cache: crate::sessions::Settings,
    #[serde(default = "default_inhibit_suspend")]
    pub inhibit_suspend: bool,
    #[serde(default)]
    pub admission: crate::admission::Settings,
    #[serde(default = "default_artifact_policy")]
    pub artifact_source_policy: crate::artifacts::SourcePolicy,
    pub resident_budget_bytes: u64,
    #[serde(
        default = "default_idle_unload_after_ms",
        skip_serializing_if = "is_default_idle_unload_after_ms"
    )]
    pub idle_unload_after_ms: u64,
    pub startup_timeout_ms: u64,
    pub request_timeout_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_settings: Option<crate::native_store::descriptor::Binding>,
    pub models: Vec<ModelSpec>,
    #[serde(default, skip_serializing_if = "ModelAliases::is_unset")]
    pub aliases: ModelAliases,
    #[serde(skip_serializing_if = "upload_maximum_is_default")]
    pub upload_max_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "crate::wyoming::Settings::is_default")]
    pub wyoming: crate::wyoming::Settings,
    #[serde(
        default,
        skip_serializing_if = "crate::protocol_access::Settings::is_default"
    )]
    pub protocols: crate::protocol_access::Settings,
    #[serde(default, skip_serializing_if = "crate::barista::Settings::is_default")]
    pub barista: crate::barista::Settings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_adoption: Option<NativeAdoption>,
}

pub const MAXIMUM_IMPORT_ROOTS: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyModelSpec {
    id: String,
    model_file: PathBuf,
    #[serde(default)]
    provider: Provider,
    #[serde(default)]
    runtime_bundle: Option<crate::runtime_bundle::BundleSpec>,
    #[serde(default)]
    options: crate::profiles::LaunchOptions,
    #[serde(default)]
    artifact: Option<String>,
    #[serde(default)]
    artifact_set: Option<String>,
    command: String,
    args: Vec<String>,
    resident_bytes: u64,
    max_inflight: u32,
    #[serde(default)]
    essential_headroom: u32,
    operations: Vec<Operation>,
    #[serde(default)]
    policy: Policy,
    #[serde(default)]
    revision: u64,
}

impl From<LegacyModelSpec> for ModelSpec {
    fn from(legacy: LegacyModelSpec) -> Self {
        Self {
            id: legacy.id,
            model_file: legacy.model_file,
            execution_profile: crate::execution_profile::ExecutionProfile::from_legacy(
                legacy.provider,
                legacy.runtime_bundle,
                legacy.options,
                legacy.command,
                legacy.args,
            ),
            resource_profile: None,
            artifact: legacy.artifact,
            artifact_set: legacy.artifact_set,
            previous_artifact_set: None,
            resident_bytes: legacy.resident_bytes,
            max_inflight: legacy.max_inflight,
            essential_headroom: legacy.essential_headroom,
            operations: legacy.operations,
            policy: legacy.policy,
            revision: legacy.revision,
            idle_unload_after_ms: None,
            companions: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ModelWire {
    Current(Box<ModelSpec>),
    Legacy(Box<LegacyModelSpec>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigWire {
    schema_version: u32,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    cache_enabled: bool,
    #[serde(default)]
    session_cache: crate::sessions::Settings,
    #[serde(default = "default_inhibit_suspend")]
    inhibit_suspend: bool,
    #[serde(default)]
    admission: crate::admission::Settings,
    #[serde(default = "default_artifact_policy")]
    artifact_source_policy: crate::artifacts::SourcePolicy,
    resident_budget_bytes: u64,
    #[serde(default = "default_idle_unload_after_ms")]
    idle_unload_after_ms: u64,
    startup_timeout_ms: u64,
    request_timeout_ms: u64,
    #[serde(default, deserialize_with = "deserialize_native_binding")]
    native_settings: Option<Box<crate::native_store::descriptor::Binding>>,
    models: Vec<ModelWire>,
    #[serde(default)]
    aliases: ModelAliases,
    #[serde(default = "default_upload_max_bytes")]
    upload_max_bytes: u64,
    #[serde(default)]
    import_roots: Vec<PathBuf>,
    #[serde(default)]
    wyoming: crate::wyoming::Settings,
    #[serde(default)]
    protocols: crate::protocol_access::Settings,
    #[serde(default)]
    barista: crate::barista::Settings,
    #[serde(default)]
    native_adoption: Option<NativeAdoption>,
}

fn deserialize_native_binding<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Box<crate::native_store::descriptor::Binding>>, D::Error> {
    Box::<crate::native_store::descriptor::Binding>::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ConfigWire::deserialize(deserializer)?;
        if !matches!(wire.schema_version, 1..=3) {
            return Err(D::Error::custom(
                "Unsupported rewrite configuration version",
            ));
        }
        let native_settings = match (wire.schema_version, wire.native_settings) {
            (3, Some(binding)) => Some(*binding),
            (1 | 2, None) => None,
            _ => {
                return Err(D::Error::custom(
                    "Native settings require one complete version-3 configuration binding",
                ));
            }
        };
        let models = wire
            .models
            .into_iter()
            .map(|model| {
                let model = match (wire.schema_version, model) {
                    (1, ModelWire::Legacy(model)) => (*model).into(),
                    (2 | 3, ModelWire::Current(model)) => *model,
                    _ => {
                        return Err(D::Error::custom(
                            "Configuration model fields do not match its schema version",
                        ));
                    }
                };
                if wire.schema_version != 3 && model.execution_profile.native_model_id().is_some() {
                    return Err(D::Error::custom(
                        "Native profiles require a version-3 configuration binding",
                    ));
                }
                model
                    .execution_profile
                    .validate()
                    .map_err(D::Error::custom)?;
                Ok(model)
            })
            .collect::<Result<_, D::Error>>()?;
        Ok(Self {
            schema_version: if wire.schema_version == 3 { 3 } else { 2 },
            native_settings,
            generation: wire.generation,
            cache_enabled: wire.cache_enabled,
            session_cache: wire.session_cache,
            inhibit_suspend: wire.inhibit_suspend,
            admission: wire.admission,
            artifact_source_policy: wire.artifact_source_policy,
            resident_budget_bytes: wire.resident_budget_bytes,
            idle_unload_after_ms: wire.idle_unload_after_ms,
            startup_timeout_ms: wire.startup_timeout_ms,
            request_timeout_ms: wire.request_timeout_ms,
            models,
            aliases: wire.aliases,
            upload_max_bytes: wire.upload_max_bytes,
            import_roots: wire.import_roots,
            wyoming: wire.wyoming,
            protocols: wire.protocols,
            barista: wire.barista,
            native_adoption: wire.native_adoption,
        })
    }
}

#[derive(Debug)]
pub struct Registration(pub ModelSpec);

impl<'de> Deserialize<'de> for Registration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let model = match ModelWire::deserialize(deserializer)? {
            ModelWire::Current(model) => *model,
            ModelWire::Legacy(model) => (*model).into(),
        };
        if model.execution_profile.native_model_id().is_some() {
            return Err(D::Error::custom(
                "Native profiles require an owned metadata import",
            ));
        }
        Ok(Self(model))
    }
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let before = fs::symlink_metadata(path)?;
        if !before.is_file()
            || before.file_type().is_symlink()
            || before.len() > MAXIMUM_CONFIG_BYTES
        {
            return Err("Configuration must be a regular file no larger than 16 MiB".into());
        }
        let file = File::open(path)?;
        let mut bytes = Vec::with_capacity(before.len() as usize);
        (&file)
            .take(MAXIMUM_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let after = fs::symlink_metadata(path)?;
        if bytes.len() as u64 > MAXIMUM_CONFIG_BYTES
            || !after.is_file()
            || after.file_type().is_symlink()
            || !crate::storage::same_file(&file, path)?
            || file.metadata()?.len() != bytes.len() as u64
            || before.modified()? != after.modified()?
        {
            return Err("Configuration changed during its bounded read".into());
        }
        Self::decode(&bytes)
    }

    pub(crate) fn read_bound(
        root: &crate::storage::DirectoryLease,
        filename: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let bytes = root.read_bytes(filename, MAXIMUM_CONFIG_BYTES)?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let config: Self = serde_json::from_slice(bytes).map_err(ConfigRecoveryError::decode)?;
        config
            .validate()
            .map_err(|_| ConfigRecoveryError::validation())?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            (self.schema_version, &self.native_settings),
            (2, None) | (3, Some(_))
        ) {
            return Err("Unsupported rewrite configuration version".into());
        }
        if self.resident_budget_bytes == 0
            || self.startup_timeout_ms == 0
            || self.request_timeout_ms == 0
        {
            return Err("Budget and timeouts must be positive".into());
        }
        self.admission
            .validate()
            .map_err(|error| error.to_string())?;
        self.session_cache.validate()?;
        if self.idle_unload_after_ms > MAXIMUM_IDLE_UNLOAD_MS
            || self.models.iter().any(|model| {
                model
                    .idle_unload_after_ms
                    .is_some_and(|value| value > MAXIMUM_IDLE_UNLOAD_MS)
            })
        {
            return Err(format!(
                "Idle unload delays must be at most {MAXIMUM_IDLE_UNLOAD_MS} milliseconds"
            ));
        }
        if self.upload_max_bytes == 0 {
            return Err("upload_max_bytes must be positive".into());
        }
        if self.import_roots.len() > MAXIMUM_IMPORT_ROOTS
            || self.import_roots.iter().any(|root| {
                !root.is_absolute()
                    || root.as_os_str().len() > 4096
                    || root.parent().is_none()
                    || root
                        .to_str()
                        .is_none_or(|text| text.chars().any(char::is_control))
            })
        {
            return Err(format!(
                "import_roots accepts at most {MAXIMUM_IMPORT_ROOTS} absolute directories other than the filesystem root"
            ));
        }
        self.wyoming.validate()?;
        self.barista.validate()?;
        self.protocols.validate()?;
        let mut ids = HashSet::new();
        let mut native_ids = HashSet::new();
        for model in &self.models {
            let profile = &model.execution_profile;
            profile.validate()?;
            if let Some(resources) = &model.resource_profile {
                resources.validate().map_err(|error| error.to_string())?;
            }
            if let Some(native_id) = profile.native_model_id()
                && (self.native_settings.is_none() || !native_ids.insert(native_id))
            {
                return Err(
                    "Native models need one unique settings binding in configuration version3"
                        .into(),
                );
            }
            if profile.native_model_id().is_some()
                && (model.id.trim() != model.id || model.id.contains(','))
            {
                return Err("Native public model IDs must be single literal aliases without surrounding whitespace or comma separators".into());
            }
            if let Some(operation) = profile.adapter.media_operation()
                && (model.max_inflight != 1 || model.operations != [operation])
            {
                return Err(format!(
                    "The selected {} worker needs one physical slot and only {}",
                    profile.adapter.implementation_id().unwrap_or_default(),
                    operation.path()
                ));
            }
            if let Some(operation) = model
                .operations
                .iter()
                .find(|operation| operation.single_slot_media())
                && profile.adapter.media_operation() != Some(*operation)
            {
                return Err(format!(
                    "{} requires its explicit single-slot media adapter",
                    operation.path()
                ));
            }
            if profile.adapter == Provider::LlamaCpp && profile.options().is_some() {
                let applied = profile
                    .resolved_arguments()
                    .map_err(|error| error.to_string())?
                    .1
                    .expect("llama options");
                if applied
                    .parallel_slots
                    .is_some_and(|slots| slots != model.max_inflight)
                {
                    return Err(
                        "The declared physical slots must match the llama.cpp launch profile"
                            .into(),
                    );
                }
            }

            if !valid_model_id(&model.id) || !ids.insert(&model.id) {
                return Err("Model IDs must be unique, nonempty and at most 256 bytes".into());
            }
            if model.model_file.as_os_str().is_empty() {
                return Err(format!("{} needs a command and model file", model.id));
            }
            if model.resident_bytes == 0
                || (self.admission.memory.mode == crate::admission::MemoryMode::Legacy
                    && model.resident_bytes > self.resident_budget_bytes)
                || model.max_inflight == 0
                || model.max_inflight > 65536
                || model.essential_headroom > model.max_inflight
            {
                return Err(format!(
                    "{} needs a positive reservation within the budget and positive request slots",
                    model.id
                ));
            }
            if let Some(set) = &model.artifact_set {
                let valid = set.strip_prefix("set-").is_some_and(|id| {
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                });
                if !valid || model.artifact.is_none() {
                    return Err("Artifact-set bindings require a canonical set identity and its explicit primary artifact".into());
                }
            }
            if let Some(previous) = &model.previous_artifact_set {
                previous.validate().map_err(|error| error.to_string())?;
                if model.artifact_set.is_none()
                    || profile.native_model_id().is_some()
                    || model.resource_profile.is_some()
                    || model.artifact_set.as_ref() == Some(&previous.artifact_set)
                {
                    return Err("A rollback binding needs a distinct complete managed set and local, unqualified resource authority".into());
                }
            }
            if model.operations.is_empty() {
                return Err(format!("{} needs explicit supported operations", model.id));
            }
            if !model.companions.is_empty() {
                let mut names = HashSet::new();
                if model.artifact_set.is_some()
                    || model.companions.len() > MAXIMUM_COMPANIONS
                    || model.companions.iter().any(|companion| {
                        !crate::runtime_arguments::valid_member_name(&companion.name)
                            || !names.insert(companion.name.as_str())
                            || !companion.path.is_absolute()
                            || companion.path.as_os_str().len() > 4096
                            || companion
                                .path
                                .to_str()
                                .is_none_or(|text| text.chars().any(char::is_control))
                    })
                {
                    return Err(format!(
                        "{} needs at most {MAXIMUM_COMPANIONS} uniquely named absolute in-place companions and no artifact set",
                        model.id
                    ));
                }
            }
        }
        if self.aliases.entries.len() > MAXIMUM_ALIASES {
            return Err(format!(
                "At most {MAXIMUM_ALIASES} model aliases are supported"
            ));
        }
        for (alias, target) in &self.aliases.entries {
            if !valid_model_id(alias) || ids.contains(alias) || !ids.contains(target) {
                return Err(
                    "Model aliases need a valid name distinct from every model ID and a registered target"
                        .into(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn check_persisted_size(&self) -> Result<(), ConfigSizeError> {
        check_serialized_size(self, MAXIMUM_CONFIG_BYTES)
    }

    pub fn resolved_models(&self, parent: &Path) -> Vec<ModelSpec> {
        self.models
            .iter()
            .map(|model| model.resolved(parent))
            .collect()
    }

    pub fn persist(
        &self,
        path: &Path,
    ) -> Result<crate::storage::AtomicCommit, crate::storage::AtomicWriteError> {
        self.check_persisted_size().map_err(|error| {
            let kind = if matches!(error, ConfigSizeError::TooLarge) {
                io::ErrorKind::InvalidData
            } else {
                io::ErrorKind::Other
            };
            crate::storage::AtomicWriteError::validation(io::Error::new(kind, error))
        })?;
        crate::storage::atomic_json(path, self)
    }
}

pub const DEFAULT_UPLOAD_MAX_BYTES: u64 = 256 * 1024 * 1024 * 1024;

fn default_upload_max_bytes() -> u64 {
    DEFAULT_UPLOAD_MAX_BYTES
}

fn upload_maximum_is_default(bytes: &u64) -> bool {
    *bytes == DEFAULT_UPLOAD_MAX_BYTES
}

fn default_inhibit_suspend() -> bool {
    true
}

pub const DEFAULT_IDLE_UNLOAD_MS: u64 = 900_000;
pub const MAXIMUM_IDLE_UNLOAD_MS: u64 = 30 * 24 * 60 * 60 * 1000;

fn default_idle_unload_after_ms() -> u64 {
    DEFAULT_IDLE_UNLOAD_MS
}

fn is_default_idle_unload_after_ms(value: &u64) -> bool {
    *value == DEFAULT_IDLE_UNLOAD_MS
}

fn default_artifact_policy() -> crate::artifacts::SourcePolicy {
    crate::artifacts::SourcePolicy::PublicHttps
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn legacy_model() -> Value {
        json!({
            "id":"folder/model λ",
            "provider":"llama-cpp",
            "model_file":"models/weights.gguf",
            "command":"./bin/engine",
            "args":["--host","127.0.0.1","--port","{port}","--model","{model}","--ctx-size=256","--parallel","1","--n-gpu-layers","42","--chat-template","line one\nline two"],
            "options":{"context_tokens":512,"threads":2},
            "resident_bytes":1024,
            "max_inflight":1,
            "operations":["chat/completions","completions"],
            "policy":"hot",
            "revision":41
        })
    }

    fn legacy_config() -> Value {
        json!({
            "schema_version":1,
            "generation":99,
            "cache_enabled":true,
            "resident_budget_bytes":4096,
            "startup_timeout_ms":1000,
            "request_timeout_ms":2000,
            "models":[legacy_model()]
        })
    }

    fn native_binding() -> crate::native_store::descriptor::Binding {
        use crate::native_store::descriptor::{Aliases, Binding, Files, Generation, Id, Presence};
        let files = Files::new(
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Aliases::RetainedAtSource,
        )
        .unwrap();
        let generation = Generation::new(Id::new("a".repeat(32)).unwrap(), files);
        Binding::new(
            Id::new("b".repeat(32)).unwrap(),
            generation.clone(),
            generation,
        )
        .unwrap()
    }

    fn native_config() -> Value {
        let local: Config = serde_json::from_value(legacy_config()).unwrap();
        let mut wire = serde_json::to_value(local).unwrap();
        wire["schema_version"] = json!(3);
        wire["native_settings"] = serde_json::to_value(native_binding()).unwrap();
        wire["models"][0]["execution_profile"] = json!({
            "schema_version":2,"native_model_id":"user.Native",
            "adapter":"llama-cpp","implementation_id":"llama-cpp",
            "command":"engine","args":["--model","{model}","--port","{port}"],
            "placement":"unresolved"
        });
        wire
    }

    #[test]
    fn model_aliases_are_optional_bounded_and_round_trip() {
        let legacy: Config = serde_json::from_value(legacy_config()).unwrap();
        assert_eq!(legacy.aliases, ModelAliases::default());
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("aliases")
                .is_none()
        );
        let mut saved = serde_json::to_value(&legacy).unwrap();
        saved["aliases"] = json!({"revision":7,"entries":{"chat":"folder/model λ"}});
        let current: Config = serde_json::from_value(saved.clone()).unwrap();
        current.validate().unwrap();
        assert_eq!(current.schema_version, 2);
        assert_eq!(current.aliases.revision, 7);
        assert_eq!(
            serde_json::to_value(&current).unwrap()["aliases"],
            saved["aliases"]
        );
        let mut legacy_with_aliases = legacy_config();
        legacy_with_aliases["aliases"] = saved["aliases"].clone();
        let migrated: Config = serde_json::from_value(legacy_with_aliases).unwrap();
        assert_eq!(migrated.aliases, current.aliases);
        let mut cleared = current.clone();
        cleared.aliases.entries.clear();
        assert_eq!(
            serde_json::to_value(&cleared).unwrap()["aliases"],
            json!({"revision":7,"entries":{}})
        );
        for (alias, target) in [
            ("folder/model λ", "folder/model λ"),
            ("chat", "absent"),
            ("", "folder/model λ"),
            ("line\nbreak", "folder/model λ"),
        ] {
            let mut bad = current.clone();
            bad.aliases.entries.clear();
            bad.aliases.entries.insert(alias.into(), target.into());
            assert!(bad.validate().is_err(), "{alias:?} -> {target:?}");
        }
        let mut crowded = current.clone();
        for index in 0..=MAXIMUM_ALIASES {
            crowded
                .aliases
                .entries
                .insert(format!("alias-{index}"), "folder/model λ".into());
        }
        assert!(crowded.validate().is_err());
        let mut unknown = saved;
        unknown["aliases"]["extra"] = json!(true);
        assert!(serde_json::from_value::<Config>(unknown).is_err());
    }

    #[test]
    fn barista_is_off_after_upgrade_and_round_trips_once_configured() {
        let upgraded: Config = serde_json::from_value(legacy_config()).unwrap();
        assert!(!upgraded.barista.enabled);
        assert_eq!(
            upgraded.barista.permission,
            crate::barista::Permission::Full
        );
        assert!(
            serde_json::to_value(&upgraded)
                .unwrap()
                .get("barista")
                .is_none()
        );
        let mut saved = serde_json::to_value(&upgraded).unwrap();
        saved["barista"] =
            json!({"enabled":true,"model":"folder/model λ","permission":"ask","revision":2});
        let current: Config = serde_json::from_value(saved.clone()).unwrap();
        current.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&current).unwrap()["barista"],
            saved["barista"]
        );
        saved["barista"] = json!({"enabled":true,"revision":2});
        let orphan: Config = serde_json::from_value(saved).unwrap();
        assert!(orphan.validate().is_err());
    }

    #[test]
    fn rollback_history_requires_distinct_complete_sets_and_cannot_replace_native_authority() {
        let original: Config = serde_json::from_value(legacy_config()).unwrap();
        assert!(
            serde_json::to_value(&original).unwrap()["models"][0]
                .get("previous_artifact_set")
                .is_none()
        );
        let mut valid = original.clone();
        let model = &mut valid.models[0];
        model.artifact = Some("a".repeat(64));
        model.artifact_set = Some(format!("set-{}", "b".repeat(64)));
        model.previous_artifact_set =
            Some(std::sync::Arc::new(crate::artifacts::ArtifactBinding {
                artifact: "a".repeat(64),
                artifact_set: format!("set-{}", "c".repeat(64)),
            }));
        valid.validate().unwrap();
        let restored: Config =
            serde_json::from_value(serde_json::to_value(&valid).unwrap()).unwrap();
        assert_eq!(
            restored.models[0].previous_artifact_set,
            valid.models[0].previous_artifact_set
        );
        for defect in ["missing-current", "same-set", "bad-digest"] {
            let mut bad = valid.clone();
            let model = &mut bad.models[0];
            match defect {
                "missing-current" => model.artifact_set = None,
                "same-set" => {
                    std::sync::Arc::make_mut(model.previous_artifact_set.as_mut().unwrap())
                        .artifact_set = model.artifact_set.clone().unwrap()
                }
                _ => {
                    std::sync::Arc::make_mut(model.previous_artifact_set.as_mut().unwrap())
                        .artifact = "../outside".into()
                }
            }
            assert!(bad.validate().is_err(), "{defect}");
        }
        let mut native: Config = serde_json::from_value(native_config()).unwrap();
        native.models[0].artifact = valid.models[0].artifact.clone();
        native.models[0].artifact_set = valid.models[0].artifact_set.clone();
        native.models[0].previous_artifact_set = valid.models[0].previous_artifact_set.clone();
        assert!(native.validate().is_err());
    }

    #[test]
    fn native_configuration_requires_one_descriptor_and_never_serializes_a_second_option_owner() {
        let wire = native_config();
        let config: Config = serde_json::from_value(wire.clone()).unwrap();
        config.validate().unwrap();
        assert_eq!(config.schema_version, 3);
        assert!(config.models[0].execution_profile.options().is_none());
        assert!(
            config.models[0]
                .execution_profile
                .resolved_arguments()
                .is_err()
        );
        let saved = serde_json::to_value(&config).unwrap();
        assert_eq!(saved["native_settings"], wire["native_settings"]);
        assert_eq!(
            saved["models"][0]["execution_profile"],
            wire["models"][0]["execution_profile"]
        );
        for value in [json!({"context_tokens":512}), Value::Null] {
            let mut bad = wire.clone();
            bad["models"][0]["execution_profile"]["options"] = value;
            assert!(serde_json::from_value::<Config>(bad).is_err());
        }
        for public_id in ["alias,another", " leading", "trailing "] {
            let mut unsupported = config.clone();
            unsupported.models[0].id = public_id.into();
            assert!(unsupported.validate().is_err());
        }
        let mut duplicate = config.clone();
        let mut model = duplicate.models[0].clone();
        model.id = "another-public-id".into();
        duplicate.models.push(model);
        assert!(duplicate.validate().is_err());
        assert!(serde_json::from_value::<Registration>(saved["models"][0].clone()).is_err());
    }

    #[test]
    fn native_descriptor_presence_and_profile_version_cannot_cross_legacy_configuration_boundaries()
    {
        let wire = native_config();
        for version in [1, 2] {
            let mut bad = wire.clone();
            bad["schema_version"] = json!(version);
            assert!(serde_json::from_value::<Config>(bad).is_err());
        }
        for presence in [None, Some(Value::Null)] {
            let mut bad = wire.clone();
            match presence {
                None => {
                    bad.as_object_mut().unwrap().remove("native_settings");
                }
                Some(value) => {
                    bad["native_settings"] = value;
                }
            }
            assert!(serde_json::from_value::<Config>(bad).is_err());
        }
        let local: Config = serde_json::from_value(legacy_config()).unwrap();
        assert_eq!(local.schema_version, 2);
        assert!(
            serde_json::to_value(local)
                .unwrap()
                .get("native_settings")
                .is_none()
        );
        let mut mixed = wire;
        mixed.as_object_mut().unwrap().remove("native_settings");
        mixed["schema_version"] = json!(2);
        assert!(serde_json::from_value::<Config>(mixed).is_err());
    }

    #[test]
    fn damaged_configuration_values_are_redacted_through_both_public_read_paths() {
        let marker = "JUICED_SYNTHETIC_CONFIG_REDACTION_MARKER";
        let mut unknown_policy = legacy_config();
        unknown_policy["artifact_source_policy"] = json!(marker);
        let mut invalid_model = legacy_config();
        invalid_model["models"][0]["id"] = json!(marker);
        invalid_model["models"][0]["resident_bytes"] = json!(0);
        let fixtures = [
            (json!(marker), "decode", Some("data")),
            (unknown_policy, "decode", Some("data")),
            (invalid_model, "validate", None),
        ];
        let directory = tempfile::tempdir().unwrap();
        let root = crate::storage::DirectoryLease::acquire(directory.path()).unwrap();
        let path = root.file_path(Path::new("config.json")).unwrap();
        for (value, stage, category) in fixtures {
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(&path, &bytes).unwrap();
            for error in [
                Config::read(&path).unwrap_err(),
                Config::read_bound(&root, Path::new("config.json")).unwrap_err(),
            ] {
                let recovery = error.downcast_ref::<ConfigRecoveryError>().unwrap();
                assert_eq!(recovery.code(), "configuration_recovery_required");
                assert_eq!(recovery.stage(), stage);
                assert_eq!(recovery.category(), category);
                assert_eq!(recovery.line().is_some(), stage == "decode");
                assert_eq!(recovery.column().is_some(), stage == "decode");
                assert!(
                    error
                        .to_string()
                        .contains("configuration_recovery_required")
                );
                assert!(format!("{error:?}").contains("configuration_recovery_required"));
                let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error.as_ref());
                while let Some(error) = cause {
                    assert!(!error.to_string().contains(marker));
                    assert!(!format!("{error:?}").contains(marker));
                    cause = error.source();
                }
                assert!(error.source().is_none());
            }
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn syntax_and_eof_recovery_keep_safe_category_and_coordinates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        for (bytes, category) in [
            (b"{\n\"schema_version\": !}".as_slice(), "syntax"),
            (b"{\n\"schema_version\":".as_slice(), "eof"),
        ] {
            fs::write(&path, bytes).unwrap();
            let error = Config::read(&path).unwrap_err();
            let recovery = error.downcast_ref::<ConfigRecoveryError>().unwrap();
            assert_eq!(recovery.stage(), "decode");
            assert_eq!(recovery.category(), Some(category));
            assert_eq!(recovery.line(), Some(2));
            assert!(recovery.column().is_some_and(|column| column > 0));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn missing_and_structural_io_failures_remain_io_errors_without_recovery_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let root = crate::storage::DirectoryLease::acquire(directory.path()).unwrap();
        let missing = root.root().join("missing.json");
        for error in [
            Config::read(&missing).unwrap_err(),
            Config::read_bound(&root, Path::new("missing.json")).unwrap_err(),
        ] {
            assert!(error.downcast_ref::<ConfigRecoveryError>().is_none());
            let io = error.downcast_ref::<io::Error>().unwrap();
            assert_eq!(io.kind(), io::ErrorKind::NotFound);
            assert!(!crate::storage::is_root_unavailable(io));
        }
        fs::create_dir(root.root().join("directory.json")).unwrap();
        let error = Config::read_bound(&root, Path::new("directory.json")).unwrap_err();
        assert!(error.downcast_ref::<ConfigRecoveryError>().is_none());
        let io = error.downcast_ref::<io::Error>().unwrap();
        assert_ne!(io.kind(), io::ErrorKind::NotFound);
        assert!(!crate::storage::is_root_unavailable(io));
        assert!(!missing.exists());
        assert!(root.root().join("directory.json").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn root_unavailability_remains_typed_and_does_not_read_the_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let root = crate::storage::DirectoryLease::acquire(directory.path().join("owned")).unwrap();
        let original = serde_json::to_vec(&legacy_config()).unwrap();
        fs::write(root.root().join("config.json"), &original).unwrap();
        let retired = directory.path().join("retired");
        fs::rename(root.root(), &retired).unwrap();
        fs::create_dir(root.root()).unwrap();
        let foreign = b"\"JUICED_SYNTHETIC_CONFIG_REDACTION_MARKER\"";
        fs::write(root.root().join("config.json"), foreign).unwrap();
        let error = Config::read_bound(&root, Path::new("config.json")).unwrap_err();
        assert!(error.downcast_ref::<ConfigRecoveryError>().is_none());
        assert!(crate::storage::is_root_unavailable(
            error.downcast_ref::<io::Error>().unwrap()
        ));
        assert_eq!(fs::read(root.root().join("config.json")).unwrap(), foreign);
        assert_eq!(fs::read(retired.join("config.json")).unwrap(), original);
        assert_eq!(fs::read_dir(root.root()).unwrap().count(), 1);
    }

    #[test]
    fn serialized_limit_includes_newline_and_actual_escaping() {
        let value = json!({"text":"quote\" slash\\ newline\n tab\t λ"});
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(encoded.len() > value["text"].as_str().unwrap().len());
        assert!(check_serialized_size(&value, encoded.len() as u64 + 1).is_ok());
        assert!(matches!(
            check_serialized_size(&value, encoded.len() as u64),
            Err(ConfigSizeError::TooLarge)
        ));
        assert!(matches!(
            check_serialized_size(&value, 0),
            Err(ConfigSizeError::TooLarge)
        ));
        assert!(check_serialized_size(&42, 3).is_ok());
        assert!(matches!(
            check_serialized_size(&42, 2),
            Err(ConfigSizeError::TooLarge)
        ));
    }

    #[test]
    fn size_count_stops_serialization_at_the_limit() {
        use serde::ser::SerializeSeq;
        struct Probe(std::cell::Cell<bool>);
        impl Serialize for Probe {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(Some(2))?;
                sequence.serialize_element("payload that cannot fit")?;
                self.0.set(true);
                sequence.serialize_element(&42)?;
                sequence.end()
            }
        }
        let probe = Probe(std::cell::Cell::new(false));
        assert!(matches!(
            check_serialized_size(&probe, 8),
            Err(ConfigSizeError::TooLarge)
        ));
        assert!(!probe.0.get());
    }

    #[test]
    fn non_limit_serialization_errors_remain_distinct() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom(
                    "controlled serialization refusal",
                ))
            }
        }
        let error = check_serialized_size(&Invalid, 1024).unwrap_err();
        assert!(matches!(error, ConfigSizeError::Serialization(_)));
        assert!(
            error
                .to_string()
                .contains("controlled serialization refusal")
        );
    }

    #[test]
    fn canonical_defaults_and_revision_growth_are_included() {
        let mut config: Config = serde_json::from_value(legacy_config()).unwrap();
        config.generation = 9;
        let canonical = serde_json::to_vec(&config).unwrap();
        let cap = canonical.len() as u64 + 1;
        assert!(check_serialized_size(&config, cap).is_ok());
        config.generation = 10;
        assert!(matches!(
            check_serialized_size(&config, cap),
            Err(ConfigSizeError::TooLarge)
        ));
        let wire = serde_json::to_value(config).unwrap();
        assert!(wire.get("admission").is_some());
        assert_eq!(wire["inhibit_suspend"], true);
    }

    #[test]
    fn cumulative_legal_profiles_cannot_publish_an_unreadable_target_or_stage_files() {
        let mut config: Config = serde_json::from_value(legacy_config()).unwrap();
        let mut model = config.models.pop().unwrap();
        model.execution_profile = crate::execution_profile::ExecutionProfile::from_legacy(
            Provider::ExternalHttp,
            None,
            Default::default(),
            "unstarted-engine".into(),
            std::iter::once("{model}".to_owned())
                .chain(std::iter::once("{port}".to_owned()))
                .chain((0..15).map(|_| "x".repeat(65536)))
                .chain(std::iter::once("x".repeat(65523)))
                .collect(),
        );
        assert_eq!(
            model
                .execution_profile
                .args
                .iter()
                .map(String::len)
                .sum::<usize>(),
            1024 * 1024
        );
        model.policy = Policy::Disabled;
        config.models = (0..17)
            .map(|index| {
                let mut model = model.clone();
                model.id = format!("bounded-{index}");
                model
            })
            .collect();
        config.validate().unwrap();
        assert!(matches!(
            config.check_persisted_size(),
            Err(ConfigSizeError::TooLarge)
        ));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, b"previous generation").unwrap();
        let error = config.persist(&path).unwrap_err();
        assert_eq!(error.stage, crate::storage::WriteStage::Validate);
        assert!(!error.committed());
        assert!(!error.root_unavailable());
        assert_eq!(fs::read(&path).unwrap(), b"previous generation");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        let absent_parent = directory.path().join("absent");
        assert_eq!(
            config
                .persist(&absent_parent.join("config.json"))
                .unwrap_err()
                .stage,
            crate::storage::WriteStage::Validate
        );
        assert!(!absent_parent.exists());
    }

    #[test]
    fn readable_legacy_input_that_canonicalizes_larger_still_reads_without_rewrite() {
        let directory = tempfile::tempdir().unwrap();
        let root = crate::storage::DirectoryLease::acquire(directory.path()).unwrap();
        let path = root.file_path(Path::new("config.json")).unwrap();
        let mut value = legacy_config();
        value["models"][0]["model_file"] = json!("");
        let overhead = serde_json::to_vec(&value).unwrap().len();
        value["models"][0]["model_file"] =
            json!("x".repeat(MAXIMUM_CONFIG_BYTES as usize - overhead));
        let original = serde_json::to_vec(&value).unwrap();
        assert_eq!(original.len() as u64, MAXIMUM_CONFIG_BYTES);
        fs::write(&path, &original).unwrap();
        let config = Config::read_bound(&root, Path::new("config.json")).unwrap();
        config.validate().unwrap();
        assert!(matches!(
            config.check_persisted_size(),
            Err(ConfigSizeError::TooLarge)
        ));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_path_serialization_rejects_before_creating_any_temporary_file() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let mut config: Config = serde_json::from_value(legacy_config()).unwrap();
        config.models[0].model_file = PathBuf::from(OsString::from_vec(vec![0xff]));
        config.validate().unwrap();
        assert!(matches!(
            config.check_persisted_size(),
            Err(ConfigSizeError::Serialization(_))
        ));
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.json");
        fs::write(&target, b"retained bytes").unwrap();
        let error = config.persist(&target).unwrap_err();
        assert_eq!(error.stage, crate::storage::WriteStage::Validate);
        assert!(!error.committed());
        assert!(!error.root_unavailable());
        assert_eq!(fs::read(target).unwrap(), b"retained bytes");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_valid_sized_configuration_preserves_ordinary_persistence_errors() {
        let config: Config = serde_json::from_value(legacy_config()).unwrap();
        config.check_persisted_size().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::create_dir(&path).unwrap();
        let error = config.persist(&path).unwrap_err();
        assert_eq!(error.stage, crate::storage::WriteStage::Rename);
        assert!(!error.committed());
        assert!(!error.root_unavailable());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn legacy_read_migrates_in_memory_without_rewriting_input_or_launch_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let input = legacy_config();
        let bytes = format!(" \n{}\n", serde_json::to_string_pretty(&input).unwrap());
        fs::write(&path, &bytes).unwrap();
        let config = Config::read(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
        assert_eq!(config.schema_version, 2);
        assert_eq!(config.generation, 99);
        assert!(config.cache_enabled);
        assert!(config.inhibit_suspend);
        assert_eq!(config.admission.startup.concurrent, 1);
        assert_eq!(config.admission.startup.pending, 64);
        assert_eq!(config.admission.startup.wait_ms, 30_000);
        let model = &config.models[0];
        assert_eq!(model.id, "folder/model λ");
        assert_eq!(model.policy, Policy::Hot);
        assert_eq!(model.revision, 41);
        assert_eq!(
            model.execution_profile.implementation_id.as_deref(),
            Some("llama-cpp")
        );
        assert_eq!(
            model.execution_profile.placement,
            crate::execution_profile::PlacementRequest::Unresolved
        );
        assert_eq!(
            serde_json::to_value(&model.execution_profile.args).unwrap(),
            input["models"][0]["args"]
        );
        assert_eq!(
            model.execution_profile.options().unwrap().context_tokens,
            Some(512)
        );
        assert_eq!(model.execution_profile.options().unwrap().threads, Some(2));
    }

    #[test]
    fn canonical_schema_two_round_trips_without_flat_launch_fields() {
        let legacy: Config = serde_json::from_value(legacy_config()).unwrap();
        let canonical = serde_json::to_value(&legacy).unwrap();
        assert_eq!(canonical["schema_version"], 2);
        let model = &canonical["models"][0];
        for field in ["provider", "runtime_bundle", "options", "command", "args"] {
            assert!(model.get(field).is_none());
        }
        assert_eq!(model["execution_profile"]["schema_version"], 1);
        assert_eq!(model["execution_profile"]["adapter"], "llama-cpp");
        assert_eq!(model["execution_profile"]["placement"], "unresolved");
        let decoded: Config = serde_json::from_value(canonical.clone()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), canonical);
    }

    #[test]
    fn startup_admission_migrates_exact_settings_and_rejects_unknown_or_duplicate_controls() {
        let mut input = legacy_config();
        input["admission"] = json!({"startup":{"concurrent":2,"pending":0,"wait_ms":42}});
        let config: Config = serde_json::from_value(input.clone()).unwrap();
        config.validate().unwrap();
        let canonical = serde_json::to_value(&config).unwrap();
        assert_eq!(
            canonical["admission"]["startup"],
            input["admission"]["startup"]
        );
        let decoded: Config = serde_json::from_value(canonical).unwrap();
        assert_eq!(decoded.admission.startup.concurrent, 2);
        assert_eq!(decoded.admission.startup.pending, 0);
        assert_eq!(decoded.admission.startup.wait_ms, 42);
        let duplicate = serde_json::to_string(&input).unwrap().replacen(
            "\"concurrent\":2",
            "\"concurrent\":2,\"concurrent\":3",
            1,
        );
        assert!(serde_json::from_str::<Config>(&duplicate).is_err());
        input["admission"]["startup"]["per_session_limit"] = json!(1);
        assert!(serde_json::from_value::<Config>(input).is_err());
    }

    #[test]
    fn unknown_versions_mixed_model_shapes_and_unknown_fields_reject() {
        let canonical =
            serde_json::to_value(serde_json::from_value::<Config>(legacy_config()).unwrap())
                .unwrap();
        let mut wrong = legacy_config();
        wrong["schema_version"] = json!(3);
        assert!(serde_json::from_value::<Config>(wrong).is_err());
        let mut wrong = legacy_config();
        wrong["models"][0] = canonical["models"][0].clone();
        assert!(serde_json::from_value::<Config>(wrong).is_err());
        let mut wrong = canonical.clone();
        wrong["models"][0] = legacy_model();
        assert!(serde_json::from_value::<Config>(wrong).is_err());
        let mut wrong = canonical.clone();
        wrong["models"].as_array_mut().unwrap().push(legacy_model());
        assert!(serde_json::from_value::<Config>(wrong).is_err());
        for (field, value) in [
            ("provider", json!("llama-cpp")),
            ("args", json!(["{model}", "{port}"])),
            ("unknown", json!(true)),
        ] {
            let mut wrong = canonical.clone();
            wrong["models"][0][field] = value;
            assert!(serde_json::from_value::<Config>(wrong).is_err());
        }
        let mut wrong = canonical;
        wrong["unknown"] = json!(true);
        assert!(serde_json::from_value::<Config>(wrong).is_err());
        let duplicate = serde_json::to_string(&legacy_config()).unwrap().replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":2",
            1,
        );
        assert!(serde_json::from_str::<Config>(&duplicate).is_err());
    }

    #[test]
    fn registration_accepts_each_exact_shape_but_rejects_mixed_and_duplicate_fields() {
        let legacy: Registration = serde_json::from_value(legacy_model()).unwrap();
        let canonical = serde_json::to_value(&legacy.0).unwrap();
        let current: Registration = serde_json::from_value(canonical.clone()).unwrap();
        assert_eq!(serde_json::to_value(current.0).unwrap(), canonical);
        let mut mixed = canonical;
        mixed["command"] = json!("different-engine");
        assert!(serde_json::from_value::<Registration>(mixed).is_err());
        let duplicate = serde_json::to_string(&legacy_model()).unwrap().replacen(
            "\"id\":",
            "\"id\":\"shadow\",\"id\":",
            1,
        );
        assert!(serde_json::from_str::<Registration>(&duplicate).is_err());
        let mut external = legacy_model();
        external.as_object_mut().unwrap().remove("provider");
        external.as_object_mut().unwrap().remove("options");
        let external: Registration = serde_json::from_value(external).unwrap();
        assert_eq!(external.0.execution_profile.adapter, Provider::ExternalHttp);
        assert!(external.0.execution_profile.implementation_id.is_none());
        assert_eq!(
            serde_json::to_value(external.0).unwrap()["execution_profile"]["implementation_id"],
            Value::Null
        );
    }

    #[test]
    fn legacy_registration_keeps_domain_validation_with_the_registration_owner() {
        let mut legacy = legacy_model();
        legacy["args"] = json!([]);
        let decoded: Registration = serde_json::from_value(legacy).unwrap();
        assert!(decoded.0.execution_profile.validate().is_err());
        let canonical = serde_json::to_value(decoded.0).unwrap();
        assert!(serde_json::from_value::<Registration>(canonical).is_err());
    }

    #[test]
    fn relative_path_resolution_preserves_option_precedence_inputs_and_bundle_identity() {
        let mut input = legacy_config();
        input["models"][0]["runtime_bundle"] = json!({
            "schema_version":1,"implementation_id":"llama-cpp","root":"runtime","entrypoint":"engine","library_directories":[],
            "files":[{"path":"engine","sha256":"a".repeat(64),"role":"executable"}]
        });
        let config: Config = serde_json::from_value(input).unwrap();
        config.validate().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let resolved = config.resolved_models(directory.path());
        let single = config.models[0].resolved(directory.path());
        assert_eq!(
            serde_json::to_value(&single).unwrap(),
            serde_json::to_value(&resolved[0]).unwrap()
        );
        let profile = &single.execution_profile;
        assert_eq!(
            resolved[0].model_file,
            directory.path().join("models/weights.gguf")
        );
        assert_eq!(
            Path::new(&profile.command),
            directory.path().join("./bin/engine")
        );
        assert_eq!(
            profile.runtime_bundle().unwrap().root,
            directory.path().join("runtime")
        );
        assert_eq!(profile.args, config.models[0].execution_profile.args);
        assert_eq!(
            profile.options(),
            config.models[0].execution_profile.options()
        );
        assert_eq!(
            profile
                .options()
                .unwrap()
                .resolve(&profile.args)
                .unwrap()
                .applied
                .context_tokens,
            Some(512)
        );
        assert_eq!(profile.args[6], "--ctx-size=256");
        let mut absolute = single.clone();
        absolute.execution_profile.command = "engine-on-path".into();
        let unchanged = absolute.resolved(&directory.path().join("different parent"));
        assert_eq!(unchanged.model_file, absolute.model_file);
        assert_eq!(unchanged.execution_profile.command, "engine-on-path");
        assert_eq!(
            unchanged.execution_profile.runtime_bundle().unwrap().root,
            absolute.execution_profile.runtime_bundle().unwrap().root
        );
        assert_eq!(
            config.models[0].model_file,
            Path::new("models/weights.gguf")
        );
    }

    #[test]
    fn migration_preserves_whisper_and_llama_slot_operation_invariants() {
        let mut input = legacy_config();
        input["models"][0]["max_inflight"] = json!(2);
        assert!(
            serde_json::from_value::<Config>(input)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut speech = legacy_config();
        speech["models"][0]["provider"] = json!("whisper-cpp");
        speech["models"][0]["options"] = json!({});
        speech["models"][0]["args"] = json!([
            "--host",
            "127.0.0.1",
            "--port",
            "{port}",
            "--model",
            "{model}",
            "--no-gpu"
        ]);
        speech["models"][0]["operations"] = json!(["audio/transcriptions"]);
        let config: Config = serde_json::from_value(speech.clone()).unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.models[0]
                .execution_profile
                .implementation_id
                .as_deref(),
            Some("whisper-cpp")
        );
        for (field, value) in [
            ("max_inflight", json!(2)),
            ("operations", json!(["completions"])),
            ("args", json!(["--port", "{port}", "--model", "{model}"])),
            (
                "args",
                json!([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "{port}",
                    "--model",
                    "{model}",
                    "--translate"
                ]),
            ),
        ] {
            let mut wrong = speech.clone();
            wrong["models"][0][field] = value;
            assert!(match serde_json::from_value::<Config>(wrong) {
                Ok(config) => config.validate().is_err(),
                Err(_) => true,
            });
        }
    }

    #[test]
    fn oversized_configuration_is_rejected_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        File::create(&path)
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
    }

    #[cfg(unix)]
    #[test]
    fn configuration_links_do_not_read_outside_the_owned_root() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let target = directory.path().join("other.json");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .to_string()
                .contains("regular file")
        );
        assert_eq!(fs::read(target).unwrap(), b"{}");
    }

    #[cfg(unix)]
    #[test]
    fn local_import_settings_default_validate_and_stay_out_of_saved_defaults() {
        let base = json!({
            "schema_version":2, "resident_budget_bytes":1, "startup_timeout_ms":1,
            "request_timeout_ms":1, "models":[],
        });
        let config: Config = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(config.upload_max_bytes, DEFAULT_UPLOAD_MAX_BYTES);
        assert!(config.import_roots.is_empty());
        let saved = serde_json::to_value(&config).unwrap();
        assert!(saved.get("upload_max_bytes").is_none() && saved.get("import_roots").is_none());
        let mut custom = base.clone();
        custom["upload_max_bytes"] = json!(1024);
        custom["import_roots"] = json!(["/data/import"]);
        let config: Config = serde_json::from_value(custom).unwrap();
        config.validate().unwrap();
        let saved = serde_json::to_value(&config).unwrap();
        assert_eq!(saved["upload_max_bytes"], 1024);
        assert_eq!(saved["import_roots"], json!(["/data/import"]));
        for (key, value) in [
            ("upload_max_bytes", json!(0)),
            ("import_roots", json!(["relative/import"])),
            ("import_roots", json!(["/"])),
        ] {
            let mut invalid = base.clone();
            invalid[key] = value;
            let config: Config = serde_json::from_value(invalid).unwrap();
            assert!(config.validate().is_err(), "{key}");
        }
    }
}
