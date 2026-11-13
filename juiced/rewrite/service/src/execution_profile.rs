use crate::{
    config::Provider, native_store::generations::PreparedBinding, profiles::LaunchOptions,
    runtime_bundle::BundleSpec,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error, ser::SerializeStruct};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlacementRequest {
    #[default]
    Unresolved,
}

#[derive(Clone, Debug)]
pub struct ExecutionProfile {
    pub schema_version: u32,
    pub adapter: Provider,
    pub implementation_id: Option<String>,
    configured_bundle: Option<BundleSpec>,
    managed_runtime: Option<Arc<ManagedRuntime>>,
    pub command: String,
    pub args: Vec<String>,
    options: Option<LaunchOptions>,
    native_model_id: Option<String>,
    native_execution: Option<Arc<NativeExecutionBinding>>,
    pub placement: PlacementRequest,
}

#[derive(Clone, Debug)]
struct ManagedRuntime {
    current: String,
    previous: Option<String>,
    binding: Option<Arc<crate::runtime_packages::Lease>>,
    previous_binding: Option<Arc<crate::runtime_packages::Lease>>,
}

struct NativeExecutionBinding {
    semantic_digest: String,
    extra_arguments: Vec<String>,
    generation: Arc<PreparedBinding>,
}

impl std::fmt::Debug for NativeExecutionBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeExecutionBinding")
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileWire {
    schema_version: u32,
    adapter: Provider,
    implementation_id: Option<String>,
    #[serde(default)]
    runtime_bundle: Option<BundleSpec>,
    #[serde(default)]
    runtime_package: Option<String>,
    #[serde(default)]
    previous_runtime_package: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    #[serde(default, deserialize_with = "present")]
    options: Option<LaunchOptions>,
    #[serde(default, deserialize_with = "present")]
    native_model_id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    placement: Option<PlacementRequest>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl Serialize for ExecutionProfile {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let native = match (
            self.schema_version,
            self.native_model_id.as_deref(),
            self.options.as_ref(),
        ) {
            (1, None, Some(_)) => false,
            (2, Some(_), _) => true,
            _ => {
                return Err(serde::ser::Error::custom(
                    "Execution profile authority does not match its schema version",
                ));
            }
        };
        let mut state = serializer.serialize_struct(
            "ExecutionProfile",
            7 + if self.managed_runtime.is_some() {
                usize::from(self.previous_runtime_package().is_some())
            } else {
                usize::from(self.configured_bundle.is_some())
            },
        )?;
        state.serialize_field("schema_version", &self.schema_version)?;
        if native {
            state.serialize_field("native_model_id", &self.native_model_id)?;
        }
        state.serialize_field("adapter", &self.adapter)?;
        state.serialize_field("implementation_id", &self.implementation_id)?;
        if let Some(package) = self.runtime_package() {
            state.serialize_field("runtime_package", package)?;
            if let Some(previous) = self.previous_runtime_package() {
                state.serialize_field("previous_runtime_package", previous)?;
            }
        } else {
            if let Some(bundle) = &self.configured_bundle {
                state.serialize_field("runtime_bundle", bundle)?;
            }
            state.serialize_field("command", &self.command)?;
        }
        state.serialize_field("args", &self.args)?;
        if !native {
            state.serialize_field(
                "options",
                self.options.as_ref().expect("local profile has options"),
            )?;
        }
        state.serialize_field("placement", &self.placement)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ExecutionProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ProfileWire::deserialize(deserializer)?;
        let (options, native_model_id, placement) = match wire.schema_version {
            1 if wire.native_model_id.is_none() => (
                Some(wire.options.unwrap_or_default()),
                None,
                wire.placement.unwrap_or_default(),
            ),
            2 if wire.options.is_none() => (
                None,
                Some(
                    wire.native_model_id
                        .ok_or_else(|| D::Error::missing_field("native_model_id"))?,
                ),
                wire.placement
                    .ok_or_else(|| D::Error::missing_field("placement"))?,
            ),
            _ => {
                return Err(D::Error::custom(
                    "Execution profile fields do not match its schema version",
                ));
            }
        };
        if wire.runtime_package.is_some()
            && (wire.command.is_some() || wire.runtime_bundle.is_some())
        {
            return Err(D::Error::custom(
                "Managed runtime profiles derive their command and bundle from the package owner",
            ));
        }
        if wire.runtime_package.is_none() && wire.previous_runtime_package.is_some() {
            return Err(D::Error::custom(
                "Runtime rollback history requires a current managed package",
            ));
        }
        if wire.runtime_package.is_none() && wire.command.is_none() {
            return Err(D::Error::missing_field("command"));
        }
        let profile = Self {
            schema_version: wire.schema_version,
            adapter: wire.adapter,
            implementation_id: wire.implementation_id,
            configured_bundle: wire.runtime_bundle,
            managed_runtime: wire.runtime_package.map(|current| {
                Arc::new(ManagedRuntime {
                    current,
                    previous: wire.previous_runtime_package,
                    binding: None,
                    previous_binding: None,
                })
            }),
            command: wire.command.unwrap_or_default(),
            args: wire.args,
            options,
            native_model_id,
            native_execution: None,
            placement,
        };
        profile.validate().map_err(D::Error::custom)?;
        Ok(profile)
    }
}

impl ExecutionProfile {
    pub fn from_legacy(
        adapter: Provider,
        runtime_bundle: Option<BundleSpec>,
        options: LaunchOptions,
        command: String,
        args: Vec<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            adapter,
            implementation_id: adapter.implementation_id().map(str::to_owned),
            configured_bundle: runtime_bundle,
            managed_runtime: None,
            command,
            args,
            options: Some(options),
            native_model_id: None,
            native_execution: None,
            placement: PlacementRequest::Unresolved,
        }
    }

    pub fn runtime_package(&self) -> Option<&str> {
        self.managed_runtime
            .as_ref()
            .map(|runtime| runtime.current.as_str())
    }
    pub fn previous_runtime_package(&self) -> Option<&str> {
        self.managed_runtime
            .as_ref()
            .and_then(|runtime| runtime.previous.as_deref())
    }
    pub(crate) fn package_binding(&self) -> Option<&Arc<crate::runtime_packages::Lease>> {
        self.managed_runtime
            .as_ref()
            .and_then(|runtime| runtime.binding.as_ref())
    }
    pub(crate) fn select_runtime_package(
        &mut self,
        id: String,
        previous: Option<String>,
    ) -> Result<(), String> {
        if self.schema_version != 1
            || self.native_model_id.is_some()
            || self.adapter == Provider::ExternalHttp
        {
            return Err("Managed runtime selection requires a local concrete adapter".into());
        }
        self.managed_runtime = Some(Arc::new(ManagedRuntime {
            current: id,
            previous,
            binding: None,
            previous_binding: None,
        }));
        self.configured_bundle = None;
        self.command.clear();
        self.validate()
    }
    pub(crate) fn bind_runtime_package(
        &mut self,
        store: Option<&Arc<crate::runtime_packages::Store>>,
        deadline: std::time::Instant,
    ) -> Result<(), crate::runtime_packages::Error> {
        let Some(runtime) = &self.managed_runtime else {
            return Ok(());
        };
        let store = store
            .ok_or_else(|| crate::runtime_packages::Error::new("package_store_unavailable"))?;
        let held = store.hold(&runtime.current, self.adapter, deadline)?;
        let previous = runtime
            .previous
            .as_ref()
            .map(|id| store.hold(id, self.adapter, deadline))
            .transpose()?;
        self.command = held
            .bundle()
            .root
            .join(&held.bundle().entrypoint)
            .to_str()
            .ok_or_else(|| crate::runtime_packages::Error::new("package_path_encoding"))?
            .to_owned();
        let runtime = Arc::make_mut(self.managed_runtime.as_mut().expect("selected runtime"));
        runtime.binding = Some(held);
        runtime.previous_binding = previous;
        Ok(())
    }

    pub fn options(&self) -> Option<&LaunchOptions> {
        self.options.as_ref()
    }

    pub fn runtime_bundle(&self) -> Option<&BundleSpec> {
        self.managed_runtime
            .as_ref()
            .and_then(|runtime| runtime.binding.as_ref())
            .map(|binding| binding.bundle())
            .or(self.configured_bundle.as_ref())
    }

    pub(crate) fn resolve_bundle_root(&mut self, parent: &std::path::Path) {
        if let Some(bundle) = &mut self.configured_bundle
            && bundle.root.is_relative()
        {
            bundle.root = parent.join(&bundle.root);
        }
    }

    pub fn native_model_id(&self) -> Option<&str> {
        self.native_model_id.as_deref()
    }

    pub fn set_local_options(&mut self, options: LaunchOptions) -> Result<(), String> {
        if self.native_model_id.is_some() || self.schema_version != 1 {
            return Err("Native profile options belong to the native settings owner".into());
        }
        self.validate_options(&options)?;
        self.options = Some(options);
        Ok(())
    }

    pub(crate) fn bind_native_options(
        &mut self,
        options: LaunchOptions,
    ) -> Result<(), crate::runtime_arguments::Error> {
        if self.schema_version != 2 || self.native_model_id.is_none() {
            return Err(runtime_error(
                "native_settings_binding_required",
                "Only a native-owned profile can bind derived native settings",
            ));
        }
        self.validate_fields()
            .map_err(|message| crate::runtime_arguments::Error {
                code: "invalid_native_profile",
                message,
            })?;
        options
            .validate()
            .map_err(|error| crate::runtime_arguments::Error {
                code: error.code,
                message: error.message,
            })?;
        self.argument_template()?.apply_options(&options)?;
        self.options = Some(options);
        self.native_execution = None;
        Ok(())
    }

    pub(crate) fn bind_native_execution(
        &mut self,
        options: LaunchOptions,
        extra_arguments: Vec<String>,
        semantic_digest: String,
        generation: Arc<PreparedBinding>,
    ) -> Result<(), crate::runtime_arguments::Error> {
        if self.schema_version != 2 || self.native_model_id.is_none() {
            return Err(runtime_error(
                "native_settings_binding_required",
                "Only a native-owned profile can bind derived native settings",
            ));
        }
        validate_native_execution_inputs(&extra_arguments, &semantic_digest)?;
        self.template_with_extras(&extra_arguments)?
            .apply_options(&options)?;
        let execution = Arc::new(NativeExecutionBinding {
            semantic_digest,
            extra_arguments,
            generation,
        });
        self.bind_native_options(options)?;
        self.native_execution = Some(execution);
        Ok(())
    }

    pub(crate) fn native_semantic_digest(&self) -> Option<&str> {
        self.native_execution
            .as_ref()
            .map(|binding| binding.semantic_digest.as_str())
    }

    pub(crate) fn native_generation(&self) -> Option<&Arc<PreparedBinding>> {
        self.native_execution
            .as_ref()
            .map(|binding| &binding.generation)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_fields()?;
        let template = self
            .argument_template()
            .map_err(|error| error.to_string())?;
        if self.adapter == Provider::LlamaCpp
            && let Some(options) = &self.options
        {
            template
                .apply_options(options)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), String> {
        match (
            self.schema_version,
            self.native_model_id.as_deref(),
            self.options.as_ref(),
        ) {
            (1, None, Some(_)) => {}
            (2, Some(id), _) if self.adapter == Provider::LlamaCpp => {
                if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                    return Err("A native profile needs a nonempty model ID of at most 256 bytes without control characters".into());
                }
            }
            (2, Some(_), _) => {
                return Err("Native-owned profiles currently require the llama-cpp adapter".into());
            }
            _ => {
                return Err(
                    "Execution profile authority does not match its supported schema version"
                        .into(),
                );
            }
        }
        if let Some(runtime) = &self.managed_runtime {
            let package = &runtime.current;
            if !crate::runtime_packages::valid_id(package)
                || self.schema_version != 1
                || self.adapter == Provider::ExternalHttp
                || runtime.previous.as_ref().is_some_and(|previous| {
                    previous == package || !crate::runtime_packages::valid_id(previous)
                })
            {
                return Err("Managed runtime selection needs distinct exact package identities and local concrete profile authority".into());
            }
        }
        if self.implementation_id.as_deref() != self.adapter.implementation_id() {
            return Err("The execution profile implementation must match its selected adapter; generic HTTP workers have no known implementation identity".into());
        }
        if let Some(bundle) = self.runtime_bundle() {
            bundle.validate(self.adapter)?;
        }
        if let Some(options) = &self.options {
            self.validate_options(options)?;
        }
        if (self.command.trim().is_empty() && self.managed_runtime.is_none())
            || self.command.len() > 4096
            || self.command.chars().any(char::is_control)
        {
            return Err("The execution profile needs a bounded command".into());
        }
        Ok(())
    }

    fn validate_options(&self, options: &LaunchOptions) -> Result<(), String> {
        options.validate().map_err(|error| error.to_string())?;
        if self.adapter != Provider::LlamaCpp && options != &LaunchOptions::default() {
            return Err("Typed llama.cpp options require an explicit llama-cpp adapter".into());
        }
        Ok(())
    }

    fn argument_template(
        &self,
    ) -> Result<crate::runtime_arguments::ArgumentTemplate, crate::runtime_arguments::Error> {
        let template = crate::runtime_arguments::ArgumentTemplate::for_profile(
            self.adapter,
            &self.command,
            &self.args,
        )?;
        if self.native_model_id.is_some()
            && (template.has_typed_options() || !template.bindings_only())
        {
            return Err(runtime_error(
                "native_settings_argument_authority",
                "Native profile arguments may contain only manager-owned model, host, port and cache bindings",
            ));
        }
        Ok(template)
    }

    fn template_with_extras(
        &self,
        extras: &[String],
    ) -> Result<crate::runtime_arguments::ArgumentTemplate, crate::runtime_arguments::Error> {
        let template = self.argument_template()?;
        if extras.is_empty() {
            return Ok(template);
        }
        let mut arguments = template.into_arguments();
        arguments.extend_from_slice(extras);
        crate::runtime_arguments::ArgumentTemplate::for_profile(
            self.adapter,
            &self.command,
            &arguments,
        )
    }

    /// Whether launches bind a llama.cpp vision projector, so chat requests can carry images.
    pub fn binds_projector(&self) -> bool {
        self.adapter == Provider::LlamaCpp
            && self.resolved_arguments().is_ok_and(|(template, _)| {
                template
                    .member_requirements()
                    .any(|(_, role)| role == crate::artifacts::ArtifactRole::Projector)
            })
    }

    pub fn resolved_arguments(
        &self,
    ) -> Result<
        (
            crate::runtime_arguments::ArgumentTemplate,
            Option<LaunchOptions>,
        ),
        crate::runtime_arguments::Error,
    > {
        if self.native_model_id.is_some() {
            self.validate_fields()
                .map_err(|message| crate::runtime_arguments::Error {
                    code: "invalid_native_profile",
                    message,
                })?;
        }
        if self.native_model_id.is_some() && self.options.is_none() {
            return Err(runtime_error(
                "native_settings_unresolved",
                "Native settings must be resolved by their owner before launching this profile",
            ));
        }
        let template = match &self.native_execution {
            Some(binding) => self.template_with_extras(&binding.extra_arguments)?,
            None => self.argument_template()?,
        };
        if self.adapter == Provider::LlamaCpp {
            let options = self.options.as_ref().ok_or_else(|| {
                runtime_error(
                    "native_settings_unresolved",
                    "Native settings must be resolved by their owner before launching this profile",
                )
            })?;
            template
                .apply_options(options)
                .map(|(template, options)| (template, Some(options)))
        } else {
            Ok((template, None))
        }
    }
}

fn validate_native_execution_inputs(
    extras: &[String],
    digest: &str,
) -> Result<(), crate::runtime_arguments::Error> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(runtime_error(
            "native_settings_identity_invalid",
            "Native execution needs an exact lowercase SHA256 semantic identity",
        ));
    }
    let mut jinja = false;
    let mut metrics = false;
    for argument in extras {
        let seen = match argument.as_str() {
            "--jinja" => &mut jinja,
            "--metrics" => &mut metrics,
            _ => {
                return Err(runtime_error(
                    "native_settings_argument_authority",
                    "Only selected native jinja and metrics controls are supported",
                ));
            }
        };
        if std::mem::replace(seen, true) {
            return Err(runtime_error(
                "native_settings_argument_authority",
                "Derived native controls cannot be repeated",
            ));
        }
    }
    Ok(())
}

fn runtime_error(code: &'static str, message: &'static str) -> crate::runtime_arguments::Error {
    crate::runtime_arguments::Error {
        code,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn profile() -> Value {
        json!({
            "schema_version":1,
            "adapter":"llama-cpp",
            "implementation_id":"llama-cpp",
            "command":"engine",
            "args":["--model", "{model}", "--port", "{port}", "--n-gpu-layers", "99"]
        })
    }

    #[test]
    fn managed_runtime_wire_owns_exact_identity_and_cannot_mix_command_bundle_or_native_authority()
    {
        let mut value = serde_json::json!({"schema_version":1,"adapter":"llama-cpp","implementation_id":"llama-cpp","runtime_package":format!("package-{}", "a".repeat(64)),"previous_runtime_package":format!("package-{}", "b".repeat(64)),"args":["--host","127.0.0.1","--port","{port}","--model","{model}"],"options":{},"placement":"unresolved"});
        value["options"] = serde_json::to_value(LaunchOptions::default()).unwrap();
        let profile: ExecutionProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(
            profile.command.is_empty()
                && profile.runtime_bundle().is_none()
                && profile.package_binding().is_none()
        );
        assert_eq!(serde_json::to_value(profile).unwrap(), value);
        for field in ["command", "runtime_bundle"] {
            let mut mixed = value.clone();
            mixed[field] = if field == "command" {
                serde_json::json!("engine")
            } else {
                serde_json::json!({})
            };
            assert!(serde_json::from_value::<ExecutionProfile>(mixed).is_err());
        }
        let mut same = value.clone();
        same["previous_runtime_package"] = same["runtime_package"].clone();
        assert!(serde_json::from_value::<ExecutionProfile>(same).is_err());
        value["runtime_package"] = serde_json::json!("package-private-path");
        assert!(serde_json::from_value::<ExecutionProfile>(value).is_err());
        let mut native = native_profile();
        native.as_object_mut().unwrap().remove("command");
        native["runtime_package"] = serde_json::json!(format!("package-{}", "a".repeat(64)));
        assert!(serde_json::from_value::<ExecutionProfile>(native).is_err());
    }

    fn native_profile() -> Value {
        json!({
            "schema_version":2,
            "native_model_id":"user.Native",
            "adapter":"llama-cpp",
            "implementation_id":"llama-cpp",
            "command":"engine",
            "args":["--model","{model}","--port","{port}","--host","127.0.0.1","--slot-save-path","{cache}"],
            "placement":"unresolved"
        })
    }

    fn derived_options() -> LaunchOptions {
        LaunchOptions {
            context_tokens: Some(2048),
            threads: Some(2),
            gpu_layers: Some(0),
            parallel_slots: Some(1),
        }
    }

    #[test]
    fn trusted_native_controls_are_exact_bounded_and_separate_from_wire_arguments() {
        let digest = "a".repeat(64);
        for extras in [
            vec![],
            vec!["--jinja".into()],
            vec!["--metrics".into()],
            vec!["--jinja".into(), "--metrics".into()],
        ] {
            validate_native_execution_inputs(&extras, &digest).unwrap();
            let profile: ExecutionProfile = serde_json::from_value(native_profile()).unwrap();
            let template = profile.template_with_extras(&extras).unwrap();
            for extra in &extras {
                assert!(template.arguments().contains(extra));
            }
            assert_eq!(serde_json::to_value(profile).unwrap(), native_profile());
        }
        for extras in [
            vec!["--jinja".into(), "--jinja".into()],
            vec!["--metrics=true".into()],
            vec!["--ctx-size".into(), "512".into()],
            vec!["--chat-template".into(), "private".into()],
        ] {
            assert_eq!(
                validate_native_execution_inputs(&extras, &digest)
                    .unwrap_err()
                    .code,
                "native_settings_argument_authority"
            );
        }
        for invalid in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
        ] {
            assert_eq!(
                validate_native_execution_inputs(&[], &invalid)
                    .unwrap_err()
                    .code,
                "native_settings_identity_invalid"
            );
        }
    }

    #[test]
    fn native_wire_has_one_explicit_authority_and_cannot_launch_before_resolution() {
        let input = native_profile();
        let profile: ExecutionProfile = serde_json::from_value(input.clone()).unwrap();
        profile.validate().unwrap();
        assert_eq!(profile.native_model_id(), Some("user.Native"));
        assert!(profile.options().is_none());
        assert!(profile.native_semantic_digest().is_none());
        assert!(profile.native_generation().is_none());
        let error = profile.resolved_arguments().unwrap_err();
        assert_eq!(error.code, "native_settings_unresolved");
        assert_eq!(serde_json::to_value(profile).unwrap(), input);
    }

    #[test]
    fn native_derived_options_are_transient_and_round_trip_back_to_unresolved() {
        let input = native_profile();
        let mut profile: ExecutionProfile = serde_json::from_value(input.clone()).unwrap();
        profile.bind_native_options(derived_options()).unwrap();
        assert!(profile.native_semantic_digest().is_none());
        assert!(profile.native_generation().is_none());
        assert_eq!(profile.options(), Some(&derived_options()));
        let (template, applied) = profile.resolved_arguments().unwrap();
        assert_eq!(applied, Some(derived_options()));
        let args = template.arguments();
        for (flag, value) in [
            ("--ctx-size", "2048"),
            ("--threads", "2"),
            ("--gpu-layers", "0"),
            ("--parallel", "1"),
        ] {
            assert!(args.windows(2).any(|pair| pair == [flag, value]));
        }
        let saved = serde_json::to_value(&profile).unwrap();
        assert_eq!(saved, input);
        assert!(saved.get("options").is_none());
        let restored: ExecutionProfile = serde_json::from_value(saved).unwrap();
        assert!(restored.options().is_none());
        assert_eq!(
            restored.resolved_arguments().unwrap_err().code,
            "native_settings_unresolved"
        );
    }

    #[test]
    fn native_profiles_reject_every_options_shape_and_mixed_authority() {
        for options in [
            Value::Null,
            json!({}),
            json!({"context_tokens":512}),
            json!([]),
            json!(false),
        ] {
            let mut value = native_profile();
            value["options"] = options;
            assert!(serde_json::from_value::<ExecutionProfile>(value).is_err());
        }
        for field in ["options", "placement"] {
            let mut local = profile();
            local[field] = Value::Null;
            assert!(serde_json::from_value::<ExecutionProfile>(local).is_err());
        }
        let mut local = profile();
        local["native_model_id"] = Value::Null;
        assert!(serde_json::from_value::<ExecutionProfile>(local).is_err());
        let mut wrong_version = native_profile();
        wrong_version["schema_version"] = json!(1);
        assert!(serde_json::from_value::<ExecutionProfile>(wrong_version).is_err());
        for missing in [
            "native_model_id",
            "placement",
            "implementation_id",
            "adapter",
            "args",
            "command",
        ] {
            let mut value = native_profile();
            value.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<ExecutionProfile>(value).is_err(),
                "{missing}"
            );
        }
        for (field, value) in [
            ("saved_options", json!({"ctx_size":512})),
            ("native_execution", Value::Null),
            ("semantic_digest", json!("a".repeat(64))),
            ("extra_arguments", json!(["--jinja"])),
        ] {
            let mut unknown = native_profile();
            unknown[field] = value;
            assert!(serde_json::from_value::<ExecutionProfile>(unknown).is_err());
        }
    }

    #[test]
    fn duplicate_native_and_local_fields_are_not_hidden_by_wire_selection() {
        for (input, key, replacement) in [
            (
                native_profile(),
                "schema_version",
                "\"schema_version\":2,\"schema_version\":2",
            ),
            (
                native_profile(),
                "native_model_id",
                "\"native_model_id\":\"user.Native\",\"native_model_id\":\"user.Other\"",
            ),
            (
                native_profile(),
                "placement",
                "\"placement\":\"unresolved\",\"placement\":\"unresolved\"",
            ),
            (
                profile(),
                "adapter",
                "\"adapter\":\"llama-cpp\",\"adapter\":\"llama-cpp\"",
            ),
        ] {
            let needle = format!("\"{key}\":{}", serde_json::to_string(&input[key]).unwrap());
            let text = serde_json::to_string(&input)
                .unwrap()
                .replacen(&needle, replacement, 1);
            assert!(
                serde_json::from_str::<ExecutionProfile>(&text).is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn native_ids_and_selected_adapter_are_bounded_without_inference() {
        for id in [
            String::new(),
            "  ".into(),
            "x".repeat(257),
            "user.\nprivate".into(),
        ] {
            let mut value = native_profile();
            value["native_model_id"] = json!(id);
            assert!(serde_json::from_value::<ExecutionProfile>(value).is_err());
        }
        let mut boundary = native_profile();
        boundary["native_model_id"] = json!("λ".repeat(128));
        assert!(serde_json::from_value::<ExecutionProfile>(boundary).is_ok());
        for adapter in ["external-http", "whisper-cpp"] {
            let mut value = native_profile();
            value["adapter"] = json!(adapter);
            value["implementation_id"] = if adapter == "external-http" {
                Value::Null
            } else {
                json!(adapter)
            };
            assert!(serde_json::from_value::<ExecutionProfile>(value).is_err());
        }
    }

    #[test]
    fn native_runtime_arguments_cannot_be_a_second_settings_authority() {
        for extra in [
            vec!["--ctx-size", "512"],
            vec!["-c", "512"],
            vec!["--threads", "4"],
            vec!["-t", "4"],
            vec!["--n-gpu-layers", "0"],
            vec!["-ngl", "0"],
            vec!["--parallel", "1"],
            vec!["-np", "1"],
            vec!["--chat-template", "--ctx-size"],
            vec!["--no-mmap"],
        ] {
            let mut value = native_profile();
            value["args"]
                .as_array_mut()
                .unwrap()
                .extend(extra.iter().map(|value| json!(value)));
            assert!(
                serde_json::from_value::<ExecutionProfile>(value).is_err(),
                "{extra:?}"
            );
        }
        let mut local = profile();
        local["args"] = json!([
            "--model",
            "{model}",
            "--port",
            "{port}",
            "--chat-template",
            "--ctx-size"
        ]);
        assert!(serde_json::from_value::<ExecutionProfile>(local).is_ok());
    }

    #[test]
    fn native_binding_and_edit_failures_preserve_the_previous_derived_view() {
        let mut profile: ExecutionProfile = serde_json::from_value(native_profile()).unwrap();
        let invalid = LaunchOptions {
            threads: Some(0),
            ..derived_options()
        };
        assert!(profile.bind_native_options(invalid.clone()).is_err());
        assert!(profile.options().is_none());
        profile.bind_native_options(derived_options()).unwrap();
        assert!(profile.set_local_options(LaunchOptions::default()).is_err());
        assert!(profile.bind_native_options(invalid).is_err());
        assert_eq!(profile.options(), Some(&derived_options()));
        profile.args.extend(["--ctx-size".into(), "4096".into()]);
        assert_eq!(
            profile
                .bind_native_options(LaunchOptions::default())
                .unwrap_err()
                .code,
            "native_settings_argument_authority"
        );
        assert_eq!(profile.options(), Some(&derived_options()));
        assert_eq!(
            profile.resolved_arguments().unwrap_err().code,
            "native_settings_argument_authority"
        );
    }

    #[test]
    fn local_options_keep_their_wire_authority_and_cannot_bind_native_settings() {
        let mut local: ExecutionProfile = serde_json::from_value(profile()).unwrap();
        assert!(local.native_model_id().is_none());
        assert_eq!(local.options(), Some(&LaunchOptions::default()));
        assert_eq!(
            local
                .bind_native_options(derived_options())
                .unwrap_err()
                .code,
            "native_settings_binding_required"
        );
        local.set_local_options(derived_options()).unwrap();
        assert!(
            local
                .set_local_options(LaunchOptions {
                    threads: Some(0),
                    ..LaunchOptions::default()
                })
                .is_err()
        );
        assert_eq!(local.options(), Some(&derived_options()));
        let saved = serde_json::to_value(local).unwrap();
        assert_eq!(saved["schema_version"], 1);
        assert_eq!(
            saved["options"],
            serde_json::to_value(derived_options()).unwrap()
        );
        assert!(saved.get("native_model_id").is_none());
    }

    #[test]
    fn selected_identity_does_not_infer_placement_or_qualification() {
        let profile: ExecutionProfile = serde_json::from_value(profile()).unwrap();
        assert_eq!(profile.placement, PlacementRequest::Unresolved);
        let saved = serde_json::to_value(profile).unwrap();
        assert_eq!(saved["placement"], "unresolved");
        assert_eq!(
            saved["options"],
            json!({"context_tokens":null,"threads":null,"gpu_layers":null,"parallel_slots":null})
        );
        assert!(saved.get("qualified").is_none());
        assert!(saved.get("supported").is_none());
        assert!(saved.get("runtime_bundle").is_none());
    }

    #[test]
    fn versions_unsupported_identity_placement_and_unknown_fields_reject() {
        for (field, value) in [
            ("schema_version", json!(2)),
            ("implementation_id", json!("candle")),
            ("implementation_id", json!("whisper-cpp")),
            ("implementation_id", Value::Null),
            ("placement", json!("cpu")),
            ("placement", json!({"device":"gpu"})),
            ("qualified", json!(true)),
            ("adapter", json!("candle")),
        ] {
            let mut value_under_test = profile();
            value_under_test[field] = value;
            assert!(serde_json::from_value::<ExecutionProfile>(value_under_test).is_err());
        }
        let mut external = profile();
        external["adapter"] = json!("external-http");
        assert!(serde_json::from_value::<ExecutionProfile>(external.clone()).is_err());
        external["implementation_id"] = Value::Null;
        let external: ExecutionProfile = serde_json::from_value(external).unwrap();
        assert!(external.implementation_id.is_none());
        assert_eq!(
            serde_json::to_value(external).unwrap()["implementation_id"],
            Value::Null
        );
    }

    #[test]
    fn argument_bounds_reject_without_rewriting_literal_inputs() {
        let mut input = profile();
        input["args"] = json!([
            "--model",
            "{model}",
            "--port",
            "{port}",
            "--chat-template",
            "line one\nline two"
        ]);
        let decoded: ExecutionProfile = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["args"],
            input["args"]
        );
        for arguments in [
            json!(["{model}", "{port}", "nul\u{0}byte"]),
            json!(["{model}", "{port}", "x".repeat(65537)]),
            json!(["{model}"]),
        ] {
            input["args"] = arguments;
            assert!(serde_json::from_value::<ExecutionProfile>(input.clone()).is_err());
        }
    }

    #[test]
    fn only_llama_profiles_with_a_projector_member_accept_images() {
        let text: ExecutionProfile = serde_json::from_value(profile()).unwrap();
        assert!(!text.binds_projector());
        let mut vision = profile();
        vision["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--mmproj"), json!("{member:mmproj}")]);
        let parsed: ExecutionProfile = serde_json::from_value(vision.clone()).unwrap();
        assert!(parsed.binds_projector());
        vision["adapter"] = json!("external-http");
        vision["implementation_id"] = json!(null);
        if let Ok(other) = serde_json::from_value::<ExecutionProfile>(vision) {
            assert!(!other.binds_projector());
        }
    }
}
