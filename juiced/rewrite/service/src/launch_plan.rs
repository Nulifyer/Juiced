use super::{Error, LaunchOptions};
use crate::{
    artifacts::ArtifactLease,
    config::{ModelSpec, Provider},
    execution_profile::PlacementRequest,
    identity::VerifiedFile,
    runtime_bundle::VerifiedBundle,
    storage::DirectoryLease,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) struct CapturedEnvironment(BTreeMap<OsString, OsString>);

impl CapturedEnvironment {
    pub(crate) fn capture(bundle: Option<&VerifiedBundle>) -> Result<Self, Error> {
        Self::from_entries(std::env::vars_os(), bundle)
    }

    fn from_entries(
        entries: impl IntoIterator<Item = (OsString, OsString)>,
        bundle: Option<&VerifiedBundle>,
    ) -> Result<Self, Error> {
        let mut values = BTreeMap::new();
        let mut bytes = 0usize;
        for (key, value) in entries {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if crate::runtime_arguments::environment_is_reserved(&name)
                || matches!(
                    name.as_str(),
                    "LD_PRELOAD" | "LD_AUDIT" | "DYLD_INSERT_LIBRARIES" | "ALL_PROXY"
                )
                || matches!(
                    name.as_str(),
                    "INVOCATION_ID"
                        | "JOURNAL_STREAM"
                        | "NOTIFY_SOCKET"
                        | "WATCHDOG_PID"
                        | "WATCHDOG_USEC"
                        | "LISTEN_PID"
                        | "LISTEN_FDS"
                        | "LISTEN_FDNAMES"
                        | "SYSTEMD_EXEC_PID"
                        | "CREDENTIALS_DIRECTORY"
                        | "RUNTIME_DIRECTORY"
                        | "STATE_DIRECTORY"
                        | "CACHE_DIRECTORY"
                        | "LOGS_DIRECTORY"
                        | "CONFIGURATION_DIRECTORY"
                )
                || name.starts_with("JUICED_")
                || name == "LEMONADE_API_KEY"
                || name == "LEMONADE_ADMIN_API_KEY"
                || (name.starts_with("LEMONADE_") && name.ends_with("_API_KEY"))
            {
                continue;
            }
            bytes = bytes.saturating_add(key.len()).saturating_add(value.len());
            if values.len() >= 4096 || bytes > 4 * 1024 * 1024 {
                return Err(Error::new(
                    "launch_environment_limit",
                    "The child environment exceeds the bounded execution profile.",
                ));
            }
            values.insert(key, value);
        }
        if let Some(bundle) = bundle {
            bundle.configure_environment(&mut values).map_err(|_| {
                Error::new(
                    "launch_environment",
                    "The bundle loader environment is unavailable.",
                )
            })?;
        }
        Ok(Self(values))
    }

    pub(crate) fn configuration_inputs(
        &self,
        profile: &crate::execution_profile::ExecutionProfile,
        deadline: std::time::Instant,
    ) -> Result<crate::runtime_inputs::ConfigurationInputs, crate::runtime_inputs::Refusal> {
        crate::runtime_inputs::ConfigurationInputs::for_profile(
            profile.adapter,
            &profile.command,
            &self.0,
            deadline,
        )
    }

    fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"juiced-child-environment-v1\0");
        for (key, value) in &self.0 {
            for bytes in [key.as_encoded_bytes(), value.as_encoded_bytes()] {
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
        format!("{:x}", hash.finalize())
    }

    fn apply(&self, command: &mut tokio::process::Command) {
        command.env_clear().envs(&self.0);
    }
}

pub(crate) struct VerifiedLaunchBindings {
    pub model: VerifiedFile,
    pub executable: VerifiedFile,
    pub bundle: Option<Arc<VerifiedBundle>>,
    pub artifact: Option<Arc<ArtifactLease>>,
    pub config_root: Option<DirectoryLease>,
    pub environment: CapturedEnvironment,
    pub port: u16,
    pub cache_directory: PathBuf,
    pub workspace: Option<tempfile::TempDir>,
    pub public_directory: Option<PathBuf>,
    pub configuration: crate::runtime_inputs::ConfigurationInputs,
}

pub(crate) struct LaunchPlan {
    model: VerifiedFile,
    executable: VerifiedFile,
    bundle: Option<Arc<VerifiedBundle>>,
    _artifact: Option<Arc<ArtifactLease>>,
    config_root: Option<DirectoryLease>,
    environment: CapturedEnvironment,
    args: Box<[String]>,
    effective: Option<LaunchOptions>,
    fingerprint: Arc<str>,
    adapter: Provider,
    implementation_id: Option<Arc<str>>,
    placement: PlacementRequest,
    workspace: Option<tempfile::TempDir>,
    cache_binding: bool,
    configuration: crate::runtime_inputs::ConfigurationInputs,
    native_generation: Option<Arc<crate::native_store::generations::PreparedBinding>>,
    runtime_package: Option<Arc<crate::runtime_packages::Lease>>,
}

#[derive(Serialize)]
pub(crate) struct Summary<'a> {
    schema_version: u32,
    adapter_contract_version: u32,
    adapter: Provider,
    implementation_id: Option<&'a str>,
    placement: PlacementRequest,
    semantic_fingerprint: &'a str,
    filesystem_immutable: bool,
    qualification: &'static str,
    runtime_configuration: Vec<crate::runtime_inputs::Summary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_set: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_package: Option<&'a str>,
}

impl LaunchPlan {
    pub(crate) fn resolve(
        spec: &ModelSpec,
        bindings: VerifiedLaunchBindings,
    ) -> Result<Self, Error> {
        let profile = &spec.execution_profile;
        profile
            .validate()
            .map_err(|message| Error::new("invalid_execution_profile", message))?;
        let set_identity = bindings
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.set_id());
        if spec.artifact_set.as_deref() != set_identity {
            return Err(Error::new(
                "artifact_set_binding_mismatch",
                "The launch needs its exact retained artifact-set binding.",
            ));
        }
        if let Some(artifact) = &bindings.artifact {
            artifact.verify_set().map_err(|_| {
                Error::new(
                    "artifact_set_changed",
                    "The retained artifact-set inputs changed before execution.",
                )
            })?;
        }
        let runtime_package = profile.package_binding().cloned();
        if profile.runtime_package() != runtime_package.as_ref().map(|held| held.id()) {
            return Err(Error::new(
                "runtime_package_unresolved",
                "The selected package needs its retained installed runtime owner before execution.",
            ));
        }
        if let Some(held) = &runtime_package {
            held.verify().map_err(|_| {
                Error::new(
                    "runtime_package_changed",
                    "The retained runtime package changed before execution.",
                )
            })?;
        }
        let native_generation = profile.native_generation().cloned();
        if profile.native_model_id().is_some() && native_generation.is_none() {
            return Err(Error::new(
                "native_settings_unresolved",
                "The native profile needs its retained settings generation before execution.",
            ));
        }
        if let Some(generation) = &native_generation {
            generation.verify().map_err(|_| {
                Error::new(
                    "native_settings_changed",
                    "The retained native settings generation changed before execution.",
                )
            })?;
        }
        if spec
            .artifact
            .as_deref()
            .is_some_and(|expected| expected != bindings.model.digest())
        {
            return Err(Error::new(
                "model_artifact_changed",
                "The managed model no longer matches its content identity.",
            ));
        }
        if bindings
            .bundle
            .as_ref()
            .is_some_and(|bundle| bundle.executable != bindings.executable.path())
        {
            return Err(Error::new(
                "bundle_entrypoint_changed",
                "The verified bundle and executable do not identify the same entrypoint.",
            ));
        }
        let (template, effective) = profile
            .resolved_arguments()
            .map_err(|error| Error::new(error.code, error.message))?;
        let public_alias = if profile.adapter == Provider::LlamaCpp {
            if spec.id.is_empty()
                || spec.id.len() > 256
                || spec.id.trim() != spec.id
                || spec.id.contains(',')
                || spec.id.chars().any(char::is_control)
            {
                return Err(Error::new(
                    "native_public_alias_unsupported",
                    "The selected llama.cpp runtime requires one literal public model alias.",
                ));
            }
            Some(spec.id.as_str())
        } else {
            None
        };
        let cache_binding = template.has_cache_binding();
        let identity = serde_json::to_vec(&(
            "juiced-launch-plan-v3",
            profile.schema_version,
            2u32,
            profile.adapter,
            &profile.implementation_id,
            bindings.model.digest(),
            bindings.executable.digest(),
            bindings
                .bundle
                .as_ref()
                .map(|bundle| bundle.digest.as_str()),
            &effective,
            (profile.native_semantic_digest(), public_alias),
            template.arguments(),
            "private-whisper-static-root-v1",
            bindings.configuration.fingerprint(),
            profile.placement,
            spec.max_inflight,
            bindings.environment.digest(),
        ))
        .map_err(|_| {
            Error::new(
                "launch_identity",
                "The semantic launch identity could not be encoded.",
            )
        })?;
        let mut digest = Sha256::new();
        digest.update(identity);
        if let Some(set) = set_identity {
            digest.update(b"\0juiced-launch-artifact-set-v1\0");
            digest.update(set.as_bytes());
        }
        if let Some(held) = &runtime_package {
            digest.update(b"\0juiced-launch-runtime-package-v1\0");
            digest.update(held.id().as_bytes());
        }
        let fingerprint = Arc::from(format!("{:x}", digest.finalize()));
        let model = bindings.model.path().as_os_str().to_str().ok_or_else(|| {
            Error::new(
                "launch_path_encoding",
                "The selected worker needs a UTF-8 model path.",
            )
        })?;
        let cache = bindings
            .cache_directory
            .as_os_str()
            .to_str()
            .ok_or_else(|| {
                Error::new(
                    "launch_path_encoding",
                    "The selected worker needs a UTF-8 snapshot path.",
                )
            })?;
        let mut args = template
            .resolve(crate::runtime_arguments::Bindings {
                model,
                port: bindings.port,
                cache,
            })
            .map_err(|error| Error::new(error.code, error.message))?;
        if let Some(alias) = public_alias {
            args.extend(["--alias".to_owned(), alias.to_owned()]);
        }
        if profile.adapter == Provider::WhisperCpp {
            let workspace = bindings.workspace.as_ref().ok_or_else(|| {
                Error::new(
                    "whisper_workspace",
                    "The speech worker needs a private workspace.",
                )
            })?;
            let public = bindings
                .public_directory
                .as_ref()
                .filter(|path| **path == workspace.path().join("empty-public"))
                .ok_or_else(|| {
                    Error::new(
                        "whisper_workspace",
                        "The speech worker needs its verified private static root.",
                    )
                })?;
            let public = public.to_str().ok_or_else(|| {
                Error::new(
                    "launch_path_encoding",
                    "The speech worker needs a UTF-8 private static root.",
                )
            })?;
            args.extend(["--public".to_owned(), public.to_owned()]);
        }
        let args = args.into_boxed_slice();
        Ok(Self {
            model: bindings.model,
            executable: bindings.executable,
            bundle: bindings.bundle,
            _artifact: bindings.artifact,
            config_root: bindings.config_root,
            environment: bindings.environment,
            args,
            effective,
            fingerprint,
            adapter: profile.adapter,
            implementation_id: profile.implementation_id.as_deref().map(Arc::from),
            placement: profile.placement,
            workspace: bindings.workspace,
            cache_binding,
            configuration: bindings.configuration,
            native_generation,
            runtime_package,
        })
    }

    pub(crate) fn verify_artifact_fresh(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), crate::runtime_identity::Refusal> {
        if self
            ._artifact
            .as_ref()
            .is_some_and(|artifact| artifact.verify_set().is_err())
        {
            return Err(crate::runtime_identity::Refusal {
                status: crate::runtime_identity::Status::Unqualified,
                code: "artifact_set_changed",
                message: "The retained artifact-set inputs changed.",
            });
        }
        self.verify_configuration_fresh(deadline)?;
        let current = self
            .model
            .verify_current()
            .and_then(|_| self.executable.verify_current())
            .and_then(|_| crate::identity::file_digest(self.model.path(), deadline))
            .and_then(|digest| self.model.verify_current().map(|_| digest));
        if !current.is_ok_and(|digest| digest == self.model.digest()) {
            return Err(crate::runtime_identity::Refusal {
                status: crate::runtime_identity::Status::Unqualified,
                code: "model_artifact_changed",
                message: "The retained model or executable changed, or its content identity could not be confirmed.",
            });
        }
        self.verify_configuration_fresh(deadline)
    }

    fn verify_configuration_fresh(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), crate::runtime_identity::Refusal> {
        if let Some(held) = &self.runtime_package {
            held.verify()
                .map_err(|_| crate::runtime_identity::Refusal {
                    status: crate::runtime_identity::Status::Unqualified,
                    code: "runtime_package_changed",
                    message: "The retained installed runtime package changed.",
                })?;
        }
        if let Some(generation) = &self.native_generation {
            generation
                .verify()
                .map_err(|_| crate::runtime_identity::Refusal {
                    status: crate::runtime_identity::Status::Unqualified,
                    code: "native_settings_changed",
                    message: "The retained native settings generation changed.",
                })?;
        }
        self.configuration.verify_fresh(deadline).map_err(|error| {
            crate::runtime_identity::Refusal {
                status: crate::runtime_identity::Status::Unqualified,
                code: error.code(),
                message: error.public_message(),
            }
        })
    }

    pub(crate) fn command(&self) -> Result<tokio::process::Command, Error> {
        if let Some(held) = &self.runtime_package {
            held.verify().map_err(|_| {
                Error::new(
                    "runtime_package_changed",
                    "The retained runtime package changed before execution.",
                )
            })?;
        }
        if let Some(generation) = &self.native_generation {
            generation.verify().map_err(|_| {
                Error::new(
                    "native_settings_changed",
                    "The retained native settings generation changed before execution.",
                )
            })?;
        }
        self.model
            .verify_current()
            .and_then(|_| self.executable.verify_current())
            .map_err(|_| {
                Error::new(
                    "launch_identity_changed",
                    "A retained launch file changed before execution.",
                )
            })?;
        if let Some(root) = &self.config_root {
            root.verify().map_err(|_| {
                Error::new(
                    "launch_storage_changed",
                    "The launch storage directory changed before execution.",
                )
            })?;
        }
        self.configuration
            .verify_fresh(std::time::Instant::now() + std::time::Duration::from_secs(1))
            .map_err(|error| Error::new(error.code(), error.public_message()))?;
        let mut command = tokio::process::Command::new(self.executable.path());
        command.args(&self.args);
        self.environment.apply(&mut command);
        if let Some(workspace) = &self.workspace {
            command.current_dir(workspace.path());
        }
        Ok(command)
    }

    pub(crate) fn has_cache_binding(&self) -> bool {
        self.cache_binding
    }

    pub(crate) fn bundle(&self) -> Option<&Arc<VerifiedBundle>> {
        self.bundle.as_ref()
    }
    pub(crate) fn executable(&self) -> &Path {
        self.executable.path()
    }
    pub(crate) fn effective(&self) -> Option<&LaunchOptions> {
        self.effective.as_ref()
    }
    pub(crate) fn fingerprint(&self) -> &Arc<str> {
        &self.fingerprint
    }
    pub(crate) fn summary(&self) -> Summary<'_> {
        Summary {
            schema_version: 1,
            adapter_contract_version: 2,
            adapter: self.adapter,
            implementation_id: self.implementation_id.as_deref(),
            placement: self.placement,
            semantic_fingerprint: &self.fingerprint,
            filesystem_immutable: false,
            qualification: "unqualified",
            runtime_configuration: self.configuration.summary(),
            runtime_package: self.runtime_package.as_ref().map(|held| held.id()),
            artifact_set: self
                ._artifact
                .as_ref()
                .and_then(|artifact| artifact.set_id()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn service_restart_metadata_is_neither_child_authority_nor_cache_identity() {
        let capture = |generation: &str| {
            CapturedEnvironment::from_entries(
                [
                    ("INVOCATION_ID".into(), generation.into()),
                    ("NOTIFY_SOCKET".into(), "/run/manager-notification".into()),
                    ("LISTEN_PID".into(), "1234".into()),
                    (
                        "CREDENTIALS_DIRECTORY".into(),
                        "/run/manager-credentials".into(),
                    ),
                    ("GGML_TEST".into(), "stable".into()),
                ],
                None,
            )
            .unwrap()
        };
        let first = capture("first");
        assert_eq!(first.0.len(), 1);
        assert_eq!(first.digest(), capture("next").digest());
    }

    #[test]
    fn captured_environment_is_sorted_bounded_and_removes_manager_authority() {
        let entries = [
            ("PATH", "/bin"),
            ("LEMONADE_API_KEY", "secret"),
            ("LEMONADE_ADMIN_API_KEY", "secret"),
            ("LEMONADE_CLOUD_API_KEY", "secret"),
            ("JUICED_CPU_PROFILE", "/tmp/profile"),
            ("LLAMA_ARG_HOST", "0.0.0.0"),
            ("LLAMA_API_KEY", "worker-secret"),
            ("HF_TOKEN", "download-secret"),
            ("HF_HUB_CACHE", "/outside/model-cache"),
            ("LD_PRELOAD", "/outside/loader.so"),
            ("DYLD_INSERT_LIBRARIES", "/outside/loader.dylib"),
            ("ALL_PROXY", "http://outside-proxy"),
            ("GGML_TEST", "one"),
        ];
        let capture = |entries: Vec<(&str, &str)>| {
            CapturedEnvironment::from_entries(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into())),
                None,
            )
            .unwrap()
        };
        let first = capture(entries.to_vec());
        assert_eq!(first.0.len(), 2);
        assert_eq!(
            first.0.get(OsStr::new("GGML_TEST")),
            Some(&OsString::from("one"))
        );
        assert_eq!(
            first.digest(),
            capture(entries.into_iter().rev().collect()).digest()
        );
        assert_ne!(
            first.digest(),
            capture(vec![("PATH", "/bin"), ("GGML_TEST", "two")]).digest()
        );
        assert!(
            CapturedEnvironment::from_entries(
                [(
                    OsString::from("X"),
                    OsString::from("a".repeat(4 * 1024 * 1024))
                )],
                None
            )
            .is_err()
        );
    }
}
