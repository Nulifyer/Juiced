use crate::{artifacts::ArtifactRole, config::Provider};
use std::{collections::VecDeque, fmt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub(crate) fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

pub struct Bindings<'a> {
    pub model: &'a str,
    pub port: u16,
    pub cache: &'a str,
    pub members: &'a [Member<'a>],
}

#[derive(Clone, Copy, Debug)]
pub struct Member<'a> {
    pub name: &'a str,
    pub role: ArtifactRole,
    pub path: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MemberBinding {
    position: usize,
    name: String,
    role: ArtifactRole,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentTemplate {
    arguments: Vec<String>,
    cache_binding: bool,
    adapter: Provider,
    fixture: bool,
    typed_spans: Vec<(usize, usize)>,
    extra_spans: Vec<(usize, usize)>,
    bindings: [Option<usize>; 4],
    members: Vec<MemberBinding>,
}

impl ArgumentTemplate {
    pub fn for_profile(
        adapter: Provider,
        command: &str,
        arguments: &[String],
    ) -> Result<Self, Error> {
        if fixture_command(command) && adapter != Provider::FastFlowLm {
            return validate_selected(adapter, arguments, true);
        }
        validate(adapter, arguments)
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn into_arguments(self) -> Vec<String> {
        self.arguments
    }

    pub fn has_cache_binding(&self) -> bool {
        self.cache_binding
    }

    pub(crate) fn has_typed_options(&self) -> bool {
        !self.typed_spans.is_empty()
    }

    /// Operator arguments that are neither manager bindings, companions nor typed options, in
    /// launch order. Switches the service itself reads from the engine are kept out of view and
    /// survive every rewrite.
    pub fn extra_arguments(&self) -> Vec<String> {
        self.extra_spans
            .iter()
            .filter(|(start, _)| !service_kept(self.adapter, &self.arguments[*start]))
            .flat_map(|(start, length)| self.arguments[*start..*start + *length].iter().cloned())
            .collect()
    }

    /// Replaces the operator arguments, keeping bindings, companions, typed options and the
    /// service-kept switches where they are.
    pub fn with_extra_arguments(&self, extras: &[String]) -> Result<Self, Error> {
        let mut position = 0;
        while let Some(token) = extras.get(position) {
            let flag = normalized_flag(
                self.adapter,
                token
                    .split_once('=')
                    .map_or(token.as_str(), |(flag, _)| flag),
            );
            if self.adapter == Provider::LlamaCpp && flag == "--no-slots" {
                return Err(Error::new(
                    "extra_argument_managed",
                    "The service reads engine slots; --no-slots cannot be set.",
                ));
            }
            if self.adapter == Provider::LlamaCpp && crate::profiles::option_field(&flag).is_some()
            {
                return Err(Error::new(
                    "extra_argument_typed",
                    "This flag has its own setting. Set it there instead of in the extra arguments.",
                ));
            }
            match passthrough(self.adapter, &flag) {
                Passthrough::ManagerOwned | Passthrough::Companion => {
                    return Err(Error::new(
                        "extra_argument_managed",
                        "The service owns the worker host, port, model, companion and cache arguments.",
                    ));
                }
                Passthrough::Accepted(arity) if !token.contains('=') => {
                    position += 1 + usize::from(arity.min(1));
                }
                Passthrough::Accepted(_) => position += 1,
                Passthrough::Forbidden | Passthrough::Unknown => {
                    return Err(Error::new(
                        "unsupported_runtime_argument",
                        "The selected engine does not accept this argument, or it changes unmanaged files, networking or lifecycle.",
                    ));
                }
            }
        }
        let mut arguments = Vec::with_capacity(self.arguments.len() + extras.len());
        let mut position = 0;
        let mut spans = self.extra_spans.iter().peekable();
        while position < self.arguments.len() {
            match spans.peek() {
                Some((start, length)) if *start == position => {
                    if service_kept(self.adapter, &self.arguments[position]) {
                        arguments.extend_from_slice(&self.arguments[position..position + length]);
                    }
                    position += length;
                    spans.next();
                }
                _ => {
                    arguments.push(self.arguments[position].clone());
                    position += 1;
                }
            }
        }
        arguments.extend_from_slice(extras);
        validate_selected(self.adapter, &arguments, self.fixture)
    }

    pub(crate) fn bindings_only(&self) -> bool {
        let pairs =
            self.bindings.iter().flatten().count() + usize::from(self.bindings[2].is_none());
        self.adapter != Provider::ExternalHttp && self.arguments.len() == pairs * 2
    }

    pub fn apply_options(
        &self,
        options: &crate::profiles::LaunchOptions,
    ) -> Result<(Self, crate::profiles::LaunchOptions), Error> {
        if self.adapter != Provider::LlamaCpp {
            return Err(Error::new(
                "typed_options_unsupported",
                "Typed llama.cpp options require the selected llama.cpp adapter.",
            ));
        }
        // Advanced option values can themselves look like typed option flags.
        let subset = self
            .typed_spans
            .iter()
            .flat_map(|(position, _)| self.arguments[*position..*position + 2].iter().cloned())
            .collect::<Vec<_>>();
        let resolved = options.resolve(&subset).map_err(|error| Error {
            code: error.code,
            message: error.message,
        })?;
        let mut groups = Vec::new();
        let mut fields: [VecDeque<usize>; crate::profiles::FIELD_COUNT] =
            std::array::from_fn(|_| VecDeque::new());
        for group in resolved.args.as_chunks::<2>().0 {
            let field = crate::profiles::option_field(&group[0]).ok_or_else(truncated)?;
            let queue = fields.get_mut(field).ok_or_else(truncated)?;
            queue.push_back(groups.len());
            groups.push(group);
        }
        let mut consumed = vec![false; groups.len()];
        let mut arguments = Vec::with_capacity(self.arguments.len() + 8);
        let mut position = 0;
        let mut spans = self.typed_spans.iter().peekable();
        while position < self.arguments.len() {
            if spans.peek().is_some_and(|(start, _)| *start == position) {
                let (_, field) = spans.next().expect("known option span");
                if let Some(group) = fields[*field].pop_front() {
                    arguments.extend_from_slice(groups[group]);
                    consumed[group] = true;
                }
                position += 2;
            } else {
                arguments.push(self.arguments[position].clone());
                position += 1;
            }
        }
        for (index, group) in groups.iter().enumerate() {
            if !consumed[index] {
                arguments.extend_from_slice(*group);
            }
        }
        let template = validate_selected(self.adapter, &arguments, self.fixture)?;
        Ok((template, resolved.applied))
    }

    pub fn resolve(&self, bindings: Bindings<'_>) -> Result<Vec<String>, Error> {
        if bindings.port == 0
            || bindings.model.is_empty()
            || (self.cache_binding && bindings.cache.is_empty())
        {
            return Err(Error::new(
                "invalid_runtime_binding",
                "The resolved worker needs a model path and a nonzero private port.",
            ));
        }
        self.verify_members(
            bindings
                .members
                .iter()
                .map(|member| (member.name, member.role)),
        )?;
        let port = bindings.port.to_string();
        let mut arguments = Vec::with_capacity(self.arguments.len());
        let mut bytes = 0usize;
        for (position, argument) in self.arguments.iter().enumerate() {
            let resolved = if self.adapter == Provider::ExternalHttp && !self.fixture {
                substitute(argument, &port, bindings.model, bindings.cache)?
            } else {
                let member = self
                    .members
                    .iter()
                    .find(|member| member.position == position)
                    .and_then(|member| {
                        bindings
                            .members
                            .iter()
                            .find(|bound| bound.name == member.name)
                    });
                let value = if let Some(member) = member {
                    if member.path.is_empty() {
                        return Err(Error::new(
                            "invalid_runtime_binding",
                            "Every bound companion needs its retained path.",
                        ));
                    }
                    member.path
                } else if self.bindings[0] == Some(position) {
                    bindings.model
                } else if self.bindings[1] == Some(position) {
                    &port
                } else if self.bindings[3] == Some(position) {
                    bindings.cache
                } else {
                    argument
                };
                if value.len() > 65536 || value.contains('\0') {
                    return Err(argument_limit());
                }
                value.to_owned()
            };
            bytes = bytes.saturating_add(resolved.len());
            if bytes > 1024 * 1024 {
                return Err(argument_limit());
            }
            arguments.push(resolved);
        }
        bounded(&arguments)?;
        Ok(arguments)
    }
}

pub(crate) fn fixture_command(command: &str) -> bool {
    #[cfg(feature = "test-fixture")]
    {
        std::path::Path::new(command)
            .file_name()
            .is_some_and(|name| name == "juiced-test-worker" || name == "juiced-test-worker.exe")
    }
    #[cfg(not(feature = "test-fixture"))]
    {
        let _ = command;
        false
    }
}

pub fn validate(adapter: Provider, arguments: &[String]) -> Result<ArgumentTemplate, Error> {
    if adapter == Provider::ExternalHttp {
        bounded(arguments)?;
        if !arguments
            .iter()
            .any(|argument| argument.contains("{model}"))
            || !arguments.iter().any(|argument| argument.contains("{port}"))
        {
            return Err(Error::new(
                "missing_runtime_binding",
                "Generic HTTP worker templates need explicit model and port bindings.",
            ));
        }
        return Ok(ArgumentTemplate {
            arguments: arguments.to_owned(),
            cache_binding: false,
            adapter,
            fixture: false,
            typed_spans: Vec::new(),
            extra_spans: Vec::new(),
            bindings: [None; 4],
            members: Vec::new(),
        });
    }
    if adapter == Provider::FastFlowLm {
        return validate_flm(arguments);
    }
    if adapter == Provider::Vllm {
        bounded(arguments)?;
        let (arguments, bindings) = crate::vllm::validate(arguments)?;
        return Ok(ArgumentTemplate {
            arguments,
            cache_binding: false,
            adapter,
            fixture: false,
            typed_spans: Vec::new(),
            extra_spans: Vec::new(),
            bindings,
            members: Vec::new(),
        });
    }
    validate_selected(adapter, arguments, false)
}

/// FLM serves a checkpoint name from its own store, so the model binding is the literal
/// positional after `serve`; the registered model file only anchors launch identity.
fn validate_flm(arguments: &[String]) -> Result<ArgumentTemplate, Error> {
    bounded(arguments)?;
    if crate::flm::checkpoint(arguments).is_none() {
        return Err(Error::new(
            "missing_runtime_binding",
            "The FLM worker needs serve followed by one name:tag checkpoint.",
        ));
    }
    let mut result = arguments[..2].to_vec();
    let mut bindings = [None; 4];
    let mut seen = std::collections::BTreeSet::new();
    let mut position = 2;
    while let Some(flag) = arguments.get(position) {
        if !seen.insert(flag.as_str()) {
            return Err(Error::new(
                "duplicate_runtime_binding",
                "An FLM worker argument was supplied more than once.",
            ));
        }
        let value = arguments.get(position + 1).map(String::as_str);
        match flag.as_str() {
            "--port" | "--host" => {
                let expected = if flag == "--port" {
                    "{port}"
                } else {
                    "127.0.0.1"
                };
                if value != Some(expected) {
                    return Err(Error::new(
                        "invalid_runtime_binding",
                        "Worker model, port, host and cache arguments must use their exact manager-owned bindings.",
                    ));
                }
                if flag == "--port" {
                    bindings[1] = Some(result.len() + 1);
                } else {
                    bindings[2] = Some(result.len() + 1);
                }
                result.extend([flag.clone(), expected.to_owned()]);
                position += 2;
            }
            _ => match crate::flm::argument(flag, value) {
                Ok(crate::flm::Arity::Flag) => {
                    result.push(flag.clone());
                    position += 1;
                }
                Ok(crate::flm::Arity::Value) => {
                    result.extend([flag.clone(), value.unwrap_or_default().to_owned()]);
                    position += 2;
                }
                Err(code) => {
                    return Err(Error::new(
                        code,
                        "The FLM worker accepts only its reviewed serve flags with valid values.",
                    ));
                }
            },
        }
    }
    if bindings[1].is_none() {
        return Err(Error::new(
            "missing_runtime_binding",
            "The selected worker needs one model binding, one port binding and its required loopback host.",
        ));
    }
    if bindings[2].is_none() {
        result.extend(["--host".into(), "127.0.0.1".into()]);
    }
    Ok(ArgumentTemplate {
        arguments: result,
        cache_binding: false,
        adapter: Provider::FastFlowLm,
        fixture: false,
        typed_spans: Vec::new(),
        extra_spans: Vec::new(),
        bindings,
        members: Vec::new(),
    })
}

pub fn environment_is_reserved(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("LLAMA_ARG_")
        || matches!(
            name.as_str(),
            "LLAMA_API_KEY"
                | "HF_TOKEN"
                | "GGML_BACKEND_PATH"
                | "WHISPER_ARG_DEVICE"
                | "MODEL_ENDPOINT"
                | "HF_ENDPOINT"
                | "LLAMA_CACHE"
                | "HF_HUB_CACHE"
                | "HUGGINGFACE_HUB_CACHE"
                | "HF_HOME"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "NO_PROXY"
                | "ESPEAK_DATA_PATH"
        )
}

fn bounded(arguments: &[String]) -> Result<(), Error> {
    if arguments.len() > 4096
        || arguments
            .iter()
            .any(|argument| argument.len() > 65536 || argument.contains('\0'))
        || arguments
            .iter()
            .try_fold(0usize, |bytes, argument| bytes.checked_add(argument.len()))
            .is_none_or(|bytes| bytes > 1024 * 1024)
    {
        return Err(argument_limit());
    }
    Ok(())
}

fn argument_limit() -> Error {
    Error::new(
        "invalid_runtime_arguments",
        "Worker arguments exceed their bounded literal argument contract.",
    )
}

fn normalized_flag(adapter: Provider, flag: &str) -> String {
    if adapter == Provider::LlamaCpp && flag.starts_with("--") {
        flag.replace('_', "-")
    } else {
        flag.to_owned()
    }
}

const KOKOROS_SERVER: &str = "openai";

fn owned(
    flag: &str,
    adapter: Provider,
    fixture: bool,
    server_phase: bool,
) -> Option<(usize, &'static str)> {
    match adapter {
        Provider::StableDiffusionCpp => match flag {
            "-m" | "--model" => Some((0, "--model")),
            "--diffusion-model" => Some((0, "--diffusion-model")),
            "--listen-port" => Some((1, "--listen-port")),
            "-l" | "--listen-ip" => Some((2, "--listen-ip")),
            _ => None,
        },
        Provider::Kokoros => match (flag, server_phase) {
            ("-m" | "--model", false) => Some((0, "--model")),
            ("--port", true) => Some((1, "--port")),
            ("--ip", true) => Some((2, "--ip")),
            _ => None,
        },
        Provider::Moonshine => match flag {
            "--model-path" => Some((0, "--model-path")),
            "--port" => Some((1, "--port")),
            _ => None,
        },
        Provider::Asr4all => match flag {
            "-m" | "--model" => Some((0, "--model")),
            "--port" => Some((1, "--port")),
            _ => None,
        },
        Provider::OnnxRuntime => match flag {
            "--model-path" => Some((0, "--model-path")),
            "--port" => Some((1, "--port")),
            _ => None,
        },
        _ => match flag {
            "-m" | "--model" => Some((0, "--model")),
            "--port" => Some((1, "--port")),
            "--host" => Some((2, "--host")),
            "--slot-save-path" if adapter == Provider::LlamaCpp || fixture => {
                Some((3, "--slot-save-path"))
            }
            _ => None,
        },
    }
}

/// Moonshine and the FUTO adapter always bind loopback and reject a host argument.
fn host_flag(adapter: Provider) -> Option<&'static str> {
    match adapter {
        Provider::StableDiffusionCpp => Some("--listen-ip"),
        Provider::Kokoros => Some("--ip"),
        Provider::Moonshine | Provider::Asr4all | Provider::OnnxRuntime => None,
        _ => Some("--host"),
    }
}

fn member_flag(flag: &str, adapter: Provider, server_phase: bool) -> Option<ArtifactRole> {
    match adapter {
        Provider::StableDiffusionCpp => match flag {
            "--vae" => Some(ArtifactRole::Vae),
            "--taesd" | "--tae" => Some(ArtifactRole::Decoder),
            "--llm" | "--qwen2vl" | "--clip_l" | "--clip_g" | "--t5xxl" => {
                Some(ArtifactRole::Encoder)
            }
            _ => None,
        },
        Provider::Kokoros if !server_phase => match flag {
            "-d" | "--data" => Some(ArtifactRole::Voice),
            _ => None,
        },
        Provider::LlamaCpp => match flag {
            "-mm" | "--mmproj" => Some(ArtifactRole::Projector),
            _ => None,
        },
        _ => None,
    }
}

pub fn valid_member_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Passthrough {
    Accepted(u8),
    ManagerOwned,
    Companion,
    Forbidden,
    Unknown,
}

/// Classifies one operator-supplied worker flag the way [`validate`] would treat it.
pub fn passthrough(adapter: Provider, flag: &str) -> Passthrough {
    let flag = normalized_flag(adapter, flag);
    if member_flag(&flag, adapter, false).is_some() {
        return Passthrough::Companion;
    }
    if owned(&flag, adapter, false, adapter == Provider::Kokoros).is_some() {
        return Passthrough::ManagerOwned;
    }
    if forbidden(&flag, adapter) {
        return Passthrough::Forbidden;
    }
    match match adapter {
        Provider::LlamaCpp => llama_arity(&flag),
        Provider::WhisperCpp => whisper_arity(&flag),
        Provider::StableDiffusionCpp => stable_diffusion_arity(&flag),
        Provider::Moonshine => (flag == "--model-arch").then_some(1),
        _ => None,
    } {
        Some(arity) => Passthrough::Accepted(arity),
        None => Passthrough::Unknown,
    }
}

pub fn member_placeholder(name: &str) -> String {
    format!("{{member:{name}}}")
}

fn member_name(value: &str) -> Option<&str> {
    let name = value.strip_prefix("{member:")?.strip_suffix('}')?;
    valid_member_name(name).then_some(name)
}

impl ArgumentTemplate {
    pub fn member_requirements(&self) -> impl Iterator<Item = (&str, ArtifactRole)> {
        self.members
            .iter()
            .map(|member| (member.name.as_str(), member.role))
    }

    /// Every companion in the retained set must be named by exactly one argument with its
    /// declared role, so a set cannot carry inputs the worker silently ignores.
    pub fn verify_members<'a>(
        &self,
        companions: impl IntoIterator<Item = (&'a str, ArtifactRole)>,
    ) -> Result<(), Error> {
        if self.adapter == Provider::Moonshine {
            // Without a retained set, the worker reads the registered file's own directory.
            let mut companions = companions.into_iter().peekable();
            if companions.peek().is_none() {
                return Ok(());
            }
            return verify_directory_members(companions);
        }
        let mismatch = || {
            Error::new(
                "artifact_set_binding_mismatch",
                "Every artifact-set companion must be bound once by a worker argument with its declared role.",
            )
        };
        let mut available = std::collections::BTreeMap::new();
        for (name, role) in companions {
            if available.insert(name, role).is_some() {
                return Err(mismatch());
            }
        }
        if available.len() != self.members.len() {
            return Err(mismatch());
        }
        for member in &self.members {
            if available.get(member.name.as_str()) != Some(&member.role) {
                return Err(mismatch());
            }
        }
        Ok(())
    }
}

/// Moonshine reads its model directory by file name, so the launch links every retained
/// member into a private directory; each must be a distinct file with a model-input role.
pub fn verify_directory_members<'a>(
    companions: impl IntoIterator<Item = (&'a str, ArtifactRole)>,
) -> Result<(), Error> {
    let mismatch = || {
        Error::new(
            "artifact_set_binding_mismatch",
            "A Moonshine set needs distinct encoder, decoder, tokenizer, configuration or processor members, including streaming_config.json and tokenizer.json.",
        )
    };
    let mut names = std::collections::BTreeMap::new();
    for (name, role) in companions {
        if !matches!(
            role,
            ArtifactRole::Encoder
                | ArtifactRole::Decoder
                | ArtifactRole::Tokenizer
                | ArtifactRole::Configuration
                | ArtifactRole::Processor
        ) || names.insert(name, role).is_some()
        {
            return Err(mismatch());
        }
    }
    if names.get(crate::engines::models::MOONSHINE_CONFIG) != Some(&ArtifactRole::Configuration)
        || names.get(crate::engines::models::MOONSHINE_TOKENIZER) != Some(&ArtifactRole::Tokenizer)
    {
        return Err(mismatch());
    }
    Ok(())
}

fn validate_selected(
    adapter: Provider,
    arguments: &[String],
    fixture: bool,
) -> Result<ArgumentTemplate, Error> {
    bounded(arguments)?;
    let mut seen = [false; 4];
    let mut bindings = [None; 4];
    let mut result = Vec::with_capacity(arguments.len() + 2);
    let mut typed_spans = Vec::new();
    let mut extra_spans = Vec::new();
    let mut members: Vec<MemberBinding> = Vec::new();
    let mut server_phase = false;
    let mut position = 0;
    while let Some(argument) = arguments.get(position) {
        if adapter == Provider::Kokoros && argument == KOKOROS_SERVER {
            if server_phase {
                return Err(Error::new(
                    "unsupported_runtime_argument",
                    "The Kokoros worker accepts its server subcommand once.",
                ));
            }
            server_phase = true;
            result.push(argument.clone());
            position += 1;
            continue;
        }
        let (raw_flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        let flag = normalized_flag(adapter, raw_flag);
        if let Some(role) = member_flag(&flag, adapter, server_phase) {
            let value = inline
                .or_else(|| arguments.get(position + 1).map(String::as_str))
                .ok_or_else(truncated)?;
            let name = member_name(value).ok_or_else(|| {
                Error::new(
                    "invalid_runtime_binding",
                    "Companion model arguments must name a retained artifact-set member as {member:NAME}.",
                )
            })?;
            if members.iter().any(|member| member.name == name) {
                return Err(Error::new(
                    "duplicate_runtime_binding",
                    "An artifact-set companion was bound more than once.",
                ));
            }
            members.push(MemberBinding {
                position: result.len() + 1,
                name: name.to_owned(),
                role,
            });
            result.extend([flag, value.to_owned()]);
            position += if inline.is_some() { 1 } else { 2 };
            continue;
        }
        if let Some((index, canonical)) = owned(&flag, adapter, fixture, server_phase) {
            if seen[index] {
                return Err(Error::new(
                    "duplicate_runtime_binding",
                    "A manager-owned worker argument was supplied more than once.",
                ));
            }
            let value = inline
                .or_else(|| arguments.get(position + 1).map(String::as_str))
                .ok_or_else(truncated)?;
            let valid = match index {
                0 => value == "{model}",
                1 => value == "{port}",
                2 => value == "127.0.0.1",
                3 => value == "{cache}",
                _ => false,
            };
            if !valid {
                return Err(Error::new(
                    "invalid_runtime_binding",
                    "Worker model, port, host and cache arguments must use their exact manager-owned bindings.",
                ));
            }
            seen[index] = true;
            bindings[index] = Some(result.len() + 1);
            result.extend([canonical.to_owned(), value.to_owned()]);
            position += if inline.is_some() { 1 } else { 2 };
            continue;
        }
        if forbidden(&flag, adapter) {
            return Err(Error::new(
                "unbound_runtime_side_input",
                "This selected worker argument changes unmanaged authority, artifacts, files, networking or lifecycle.",
            ));
        }
        let fixture_flag = fixture && fixture_arity(&flag).is_some();
        let arity = if fixture { fixture_arity(&flag) } else { None }
            .or_else(|| match adapter {
                Provider::LlamaCpp => llama_arity(&flag),
                Provider::WhisperCpp => whisper_arity(&flag),
                Provider::StableDiffusionCpp => stable_diffusion_arity(&flag),
                Provider::Kokoros if !server_phase => kokoros_arity(&flag),
                Provider::Parakeet => {
                    matches!(flag.as_str(), "--threads" | "--concurrency").then_some(1)
                }
                Provider::Moonshine => (flag == "--model-arch").then_some(1),
                Provider::Asr4all => matches!(
                    flag.as_str(),
                    "-t" | "--threads" | "--futo-commit" | "--futo-right"
                )
                .then_some(1),
                Provider::Ds4 => crate::vllm::ds4_arity(&flag),
                Provider::OnnxRuntime => (flag == "--verbose").then_some(0),
                Provider::Kokoros
                | Provider::ExternalHttp
                | Provider::FastFlowLm
                | Provider::Vllm => None,
            })
            .ok_or_else(|| {
                Error::new(
                    "unsupported_runtime_argument",
                    "The selected adapter does not define this worker argument or positional form.",
                )
            })?;
        let start = result.len();
        if adapter == Provider::LlamaCpp
            && let Some(field) = crate::profiles::option_field(&flag)
        {
            typed_spans.push((start, field));
        } else if !fixture_flag {
            extra_spans.push((start, usize::from(arity.min(1)) + 1));
        }
        result.push(flag);
        if arity == 0 {
            if inline.is_some() {
                return Err(truncated());
            }
            position += 1;
        } else if let Some(value) = inline {
            result.push(value.to_owned());
            position += 1;
        } else {
            let value = arguments.get(position + 1).ok_or_else(truncated)?;
            result.push(value.clone());
            position += 2;
        }
    }
    if !seen[0] || !seen[1] || (adapter == Provider::WhisperCpp && !seen[2]) {
        return Err(Error::new(
            "missing_runtime_binding",
            "The selected worker needs one model binding, one port binding and its required loopback host.",
        ));
    }
    if adapter == Provider::Kokoros
        && !members
            .iter()
            .any(|member| member.role == ArtifactRole::Voice)
    {
        return Err(Error::new(
            "missing_runtime_binding",
            "The Kokoros worker needs its server subcommand and one voices companion binding.",
        ));
    }
    if !seen[2]
        && let Some(flag) = host_flag(adapter)
    {
        result.extend([flag.into(), "127.0.0.1".into()]);
    }
    bounded(&result)?;
    Ok(ArgumentTemplate {
        arguments: result,
        cache_binding: seen[3],
        adapter,
        fixture,
        typed_spans,
        extra_spans,
        bindings,
        members,
    })
}

fn service_kept(adapter: Provider, flag: &str) -> bool {
    adapter == Provider::LlamaCpp && matches!(flag, "--slots" | "--metrics")
}

fn truncated() -> Error {
    Error::new(
        "invalid_runtime_arguments",
        "The selected worker argument needs its complete declared value form.",
    )
}

fn forbidden(flag: &str, adapter: Provider) -> bool {
    if matches!(flag, "--" | "-h" | "--help" | "--usage" | "--version") {
        return true;
    }
    match adapter {
        Provider::ExternalHttp
        | Provider::FastFlowLm
        | Provider::Vllm
        | Provider::Ds4
        | Provider::OnnxRuntime => false,
        Provider::StableDiffusionCpp => matches!(
            flag,
            "--serve-html-path"
                | "--lora-model-dir"
                | "--embd-dir"
                | "--hires-upscalers-dir"
                | "--upscale-model"
                | "--control-net"
                | "--photo-maker"
                | "--pulid-weights"
                | "--ip-adapter"
                | "--motion-module"
                | "--rpc-servers"
                | "--tokenizer"
                | "--prompt-file"
                | "--negative-prompt-file"
                | "-i"
                | "--init-img"
                | "--mask"
                | "--control-image"
                | "--params-backend"
                | "--list-devices"
        ),
        Provider::Kokoros => matches!(flag, "--timestamps"),
        Provider::Parakeet => matches!(flag, "--cache-dir" | "--sound-model"),
        Provider::Moonshine => matches!(flag, "--ws-port" | "--tcp-port"),
        Provider::Asr4all => false,
        Provider::WhisperCpp => matches!(
            flag,
            "-tr"
                | "--translate"
                | "--request-path"
                | "--inference-path"
                | "--convert"
                | "--tmp-dir"
                | "--public"
                | "-vm"
                | "--vad-model"
                | "--vad"
                | "-debug"
                | "--debug-mode"
                | "-fp"
                | "--font-path"
        ),
        Provider::LlamaCpp => {
            flag.starts_with("--models-")
                || flag.starts_with("--spec-draft-")
                || matches!(
                    flag,
                    "-mu"
                        | "--model-url"
                        | "-dr"
                        | "--docker-repo"
                        | "-hf"
                        | "-hfr"
                        | "--hf-repo"
                        | "-hff"
                        | "--hf-file"
                        | "-hft"
                        | "--hf-token"
                        | "-hfd"
                        | "-hfrd"
                        | "--hf-repo-draft"
                        | "-md"
                        | "--model-draft"
                        | "-mm"
                        | "--mmproj"
                        | "-mmu"
                        | "--mmproj-url"
                        | "--mmproj-auto"
                        | "--lora"
                        | "--lora-scaled"
                        | "--lora-init-without-apply"
                        | "--control-vector"
                        | "--control-vector-scaled"
                        | "--control-vector-layer-range"
                        | "-lcs"
                        | "--lookup-cache-static"
                        | "-lcd"
                        | "--lookup-cache-dynamic"
                        | "--grammar-file"
                        | "-jf"
                        | "--json-schema-file"
                        | "--chat-template-file"
                        | "--rpc"
                        | "--connect"
                        | "--connect-code"
                        | "--tools"
                        | "--tools-runtime"
                        | "--mcp-servers-config"
                        | "--mcp-servers-json"
                        | "-ag"
                        | "--agent"
                        | "-no-ag"
                        | "--no-agent"
                        | "--ui-mcp-proxy"
                        | "--webui-mcp-proxy"
                        | "--no-ui-mcp-proxy"
                        | "--no-webui-mcp-proxy"
                        | "--ui-config"
                        | "--webui-config"
                        | "--ui-config-file"
                        | "--webui-config-file"
                        | "--api-key"
                        | "--api-key-file"
                        | "--ssl-key-file"
                        | "--ssl-cert-file"
                        | "--api-prefix"
                        | "--reuse-port"
                        | "--no-models-autoload"
                        | "--path"
                        | "--media-path"
                        | "--log-file"
                        | "--log-prompts-dir"
                        | "--sleep-idle-seconds"
                        | "--cache-list"
                        | "-cl"
                        | "--completion-bash"
                        | "--list-devices"
                        | "--embd-gemma-default"
                        | "--fim-qwen-1.5b-default"
                        | "--fim-qwen-3b-default"
                        | "--fim-qwen-7b-default"
                        | "--fim-qwen-7b-spec"
                        | "--fim-qwen-14b-spec"
                        | "--fim-qwen-30b-default"
                        | "--gpt-oss-20b-default"
                        | "--gpt-oss-120b-default"
                        | "--vision-gemma-4b-default"
                        | "--vision-gemma-12b-default"
                )
        }
    }
}

fn llama_arity(flag: &str) -> Option<u8> {
    if crate::hybrid::flag(flag) {
        return Some(1);
    }
    const SWITCHES: &[&str] = &[
        "--swa-full",
        "-kvu",
        "--kv-unified",
        "-no-kvu",
        "--no-kv-unified",
        "--cache-idle-slots",
        "--no-cache-idle-slots",
        "--context-shift",
        "--no-context-shift",
        "--perf",
        "--no-perf",
        "-e",
        "--escape",
        "--no-escape",
        "-sp",
        "--special",
        "--warmup",
        "--no-warmup",
        "--spm-infill",
        "--ignore-eos",
        "-bs",
        "--backend-sampling",
        "-kvo",
        "--kv-offload",
        "-nkvo",
        "--no-kv-offload",
        "--repack",
        "-nr",
        "--no-repack",
        "--no-host",
        "-cb",
        "--cont-batching",
        "-nocb",
        "--no-cont-batching",
        "--no-mmproj",
        "--no-mmproj-auto",
        "--no-mmproj-offload",
        "-cmoe",
        "--cpu-moe",
        "--check-tensors",
        "--op-offload",
        "--no-op-offload",
        "--no-ui",
        "--no-webui",
        "--embedding",
        "--embeddings",
        "--rerank",
        "--reranking",
        "--cache-prompt",
        "--no-cache-prompt",
        "--metrics",
        "--props",
        "--slots",
        "--no-slots",
        "--jinja",
        "--no-jinja",
        "--reasoning-preserve",
        "--no-reasoning-preserve",
        "--skip-chat-parsing",
        "--no-skip-chat-parsing",
        "--prefill-assistant",
        "--no-prefill-assistant",
        "--log-disable",
        "--log-jsonl",
        "--no-log-jsonl",
        "-v",
        "--verbose",
        "--log-verbose",
        "--offline",
        "--log-prefix",
        "--no-log-prefix",
        "--log-timestamps",
        "--no-log-timestamps",
    ];
    const VALUES: &[&str] = &[
        "-t",
        "--threads",
        "-tb",
        "--threads-batch",
        "-C",
        "--cpu-mask",
        "-Cr",
        "--cpu-range",
        "--cpu-strict",
        "--prio",
        "--poll",
        "-Cb",
        "--cpu-mask-batch",
        "-Crb",
        "--cpu-range-batch",
        "--cpu-strict-batch",
        "--prio-batch",
        "--poll-batch",
        "-c",
        "--ctx-size",
        "--kv-unified-per-slot",
        "-n",
        "--predict",
        "--n-predict",
        "-b",
        "--batch-size",
        "-ub",
        "--ubatch-size",
        "--keep",
        "-ctxcp",
        "--ctx-checkpoints",
        "--swa-checkpoints",
        "-cms",
        "--checkpoint-min-step",
        "-cram",
        "--cache-ram",
        "-fa",
        "--flash-attn",
        "-r",
        "--reverse-prompt",
        "--samplers",
        "-s",
        "--seed",
        "--sampler-seq",
        "--sampling-seq",
        "--temp",
        "--temperature",
        "--top-k",
        "--top-p",
        "--min-p",
        "--top-nsigma",
        "--top-n-sigma",
        "--xtc-probability",
        "--xtc-threshold",
        "--typical",
        "--typical-p",
        "--repeat-last-n",
        "--repeat-penalty",
        "--presence-penalty",
        "--frequency-penalty",
        "--dry-multiplier",
        "--dry-base",
        "--dry-allowed-length",
        "--dry-penalty-last-n",
        "--dry-sequence-breaker",
        "--adaptive-target",
        "--adaptive-decay",
        "--dynatemp-range",
        "--dynatemp-exp",
        "--mirostat",
        "--mirostat-lr",
        "--mirostat-ent",
        "-l",
        "--logit-bias",
        "--grammar",
        "-j",
        "--json-schema",
        "--pooling",
        "--rope-scaling",
        "--rope-scale",
        "--rope-freq-base",
        "--rope-freq-scale",
        "--yarn-orig-ctx",
        "--yarn-ext-factor",
        "--yarn-attn-factor",
        "--yarn-beta-slow",
        "--yarn-beta-fast",
        "-ctk",
        "--cache-type-k",
        "-ctv",
        "--cache-type-v",
        "-dt",
        "--defrag-thold",
        "-np",
        "--parallel",
        "--mtmd-batch-max-tokens",
        "-lm",
        "--load-mode",
        "-lzm",
        "--lazy-mode",
        "--numa",
        "-dev",
        "--device",
        "-ot",
        "--override-tensor",
        "-ncmoe",
        "--n-cpu-moe",
        "--moe-cache-mib",
        "-ncffn",
        "--n-cpu-ffn",
        "-ngl",
        "--gpu-layers",
        "--n-gpu-layers",
        "-sm",
        "--split-mode",
        "-ts",
        "--tensor-split",
        "-mg",
        "--main-gpu",
        "-fit",
        "--fit",
        "-fitt",
        "--fit-target",
        "-fitc",
        "--fit-ctx",
        "--override-kv",
        "-a",
        "--alias",
        "--tags",
        "--embd-normalize",
        "--cors-origins",
        "--cors-methods",
        "--cors-headers",
        "--chat-template-kwargs",
        "-to",
        "--timeout",
        "--sse-ping-interval",
        "--threads-http",
        "--cache-reuse",
        "--reasoning-format",
        "-rea",
        "--reasoning",
        "--reasoning-effort",
        "--reasoning-budget",
        "--reasoning-budget-message",
        "--chat-template",
        "-sps",
        "--slot-prompt-similarity",
        "--log-colors",
        "-lv",
        "--verbosity",
        "--log-verbosity",
        "--spec-type",
        "--spec-ngram-mod-n-min",
        "--spec-ngram-mod-n-max",
        "--spec-ngram-mod-n-match",
        "--spec-ngram-simple-size-n",
        "--spec-ngram-simple-size-m",
        "--spec-ngram-simple-min-hits",
        "--spec-ngram-map-k-size-n",
        "--spec-ngram-map-k-size-m",
        "--spec-ngram-map-k-min-hits",
        "--spec-ngram-map-k4v-size-n",
        "--spec-ngram-map-k4v-size-m",
        "--spec-ngram-map-k4v-min-hits",
        "--spec-ngram-size-n",
        "--spec-ngram-size-m",
        "--spec-ngram-min-hits",
    ];
    if SWITCHES.contains(&flag) {
        Some(0)
    } else if VALUES.contains(&flag) {
        Some(1)
    } else {
        None
    }
}

fn whisper_arity(flag: &str) -> Option<u8> {
    const SWITCHES: &[&str] = &[
        "-di",
        "--diarize",
        "-tdrz",
        "--tinydiarize",
        "-sow",
        "--split-on-word",
        "-nf",
        "--no-fallback",
        "-ps",
        "--print-special",
        "-pc",
        "--print-colors",
        "-pr",
        "--print-realtime",
        "-pp",
        "--print-progress",
        "-nt",
        "--no-timestamps",
        "-dl",
        "--detect-language",
        "-ng",
        "--no-gpu",
        "-fa",
        "--flash-attn",
        "-nfa",
        "--no-flash-attn",
        "-sns",
        "--suppress-nst",
        "-nlp",
        "--no-language-probabilities",
    ];
    const VALUES: &[&str] = &[
        "-t",
        "--threads",
        "-p",
        "--processors",
        "-ot",
        "--offset-t",
        "-on",
        "--offset-n",
        "-d",
        "--duration",
        "-mc",
        "--max-context",
        "-ml",
        "--max-len",
        "-bo",
        "--best-of",
        "-bs",
        "--beam-size",
        "-ac",
        "--audio-ctx",
        "-wt",
        "--word-thold",
        "-et",
        "--entropy-thold",
        "-lpt",
        "--logprob-thold",
        "-l",
        "--language",
        "--prompt",
        "-oved",
        "--ov-e-device",
        "-dtw",
        "--dtw",
        "-dev",
        "--device",
        "-nth",
        "--no-speech-thold",
    ];
    if SWITCHES.contains(&flag) {
        Some(0)
    } else if VALUES.contains(&flag) {
        Some(1)
    } else {
        None
    }
}

fn stable_diffusion_arity(flag: &str) -> Option<u8> {
    const SWITCHES: &[&str] = &[
        "--offload-to-cpu",
        "--mmap",
        "--fa",
        "--diffusion-fa",
        "--sage-attn",
        "--diffusion-conv-direct",
        "--vae-conv-direct",
        "--vae-tiling",
        "--force-sdxl-vae-conv-scale",
        "--eager-load",
        "--disable-prefetch",
        "--disable-segmented-compute",
        "--clip-on-cpu",
        "--vae-on-cpu",
        "--disable-image-metadata",
        "--color",
        "-v",
        "--verbose",
    ];
    const VALUES: &[&str] = &[
        "-t",
        "--threads",
        "--type",
        "--rng",
        "--sampler-rng",
        "--prediction",
        "--vae-format",
        "--backend",
        "--split-mode",
        "--max-vram",
        "--auto-fit",
        "--batched-cfg",
        "--conditioning-cache-size",
        "--log-level",
        "-H",
        "--height",
        "-W",
        "--width",
        "--steps",
        "--clip-skip",
        "--cfg-scale",
        "--img-cfg-scale",
        "--guidance",
        "--flow-shift",
        "--eta",
        "--timestep-shift",
        "--slg-scale",
        "-s",
        "--seed",
        "--sampling-method",
        "--scheduler",
        "--vae-tile-size",
        "--vae-tile-overlap",
        "--vae-relative-tile-size",
    ];
    if SWITCHES.contains(&flag) {
        Some(0)
    } else if VALUES.contains(&flag) {
        Some(1)
    } else {
        None
    }
}

fn kokoros_arity(flag: &str) -> Option<u8> {
    match flag {
        "--mono" => Some(0),
        "-l" | "--lan" | "-s" | "--style" | "-p" | "--speed" | "--instances"
        | "--initial-silence" => Some(1),
        _ => None,
    }
}

fn fixture_arity(flag: &str) -> Option<u8> {
    #[cfg(feature = "test-fixture")]
    {
        match flag {
            "--finite-stream" | "--report-timings" | "--emulate-prompt-cache" => Some(0),
            "--parallel"
            | "-np"
            | "--ctx-size"
            | "-c"
            | "--events"
            | "--mode"
            | "--startup-delay-ms"
            | "--startup-release-file"
            | "--identity-map"
            | "--inference-delay-ms"
            | "--slot-operation-delay-ms"
            | "--slot-state-overhead-bytes"
            | "--speech-response-mode"
            | "--speech-release-file"
            | "--speech-physical-release-file"
            | "--speech-status"
            | "--startup-log"
            | "--chatty-bytes"
            | "--capture-bodies" => Some(1),
            _ => None,
        }
    }
    #[cfg(not(feature = "test-fixture"))]
    {
        let _ = flag;
        None
    }
}

fn substitute(argument: &str, port: &str, model: &str, cache: &str) -> Result<String, Error> {
    let mut output = String::with_capacity(argument.len());
    let mut remaining = argument;
    while let Some(position) = remaining.find('{') {
        append(&mut output, &remaining[..position])?;
        remaining = &remaining[position..];
        let replacement = [("{port}", port), ("{model}", model), ("{cache}", cache)]
            .into_iter()
            .find(|(name, _)| remaining.starts_with(name));
        if let Some((name, value)) = replacement {
            append(&mut output, value)?;
            remaining = &remaining[name.len()..];
        } else {
            append(&mut output, "{")?;
            remaining = &remaining[1..];
        }
    }
    append(&mut output, remaining)?;
    Ok(output)
}

fn append(output: &mut String, value: &str) -> Result<(), Error> {
    if value.contains('\0') || value.len() > 65536usize.saturating_sub(output.len()) {
        return Err(argument_limit());
    }
    output.push_str(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::LaunchOptions;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn base() -> Vec<String> {
        args(&["--model", "{model}", "--port", "{port}"])
    }

    #[test]
    fn selected_bindings_normalize_aliases_inline_values_and_legacy_loopback() {
        let template = validate(
            Provider::LlamaCpp,
            &args(&["-m={model}", "--port={port}", "--slot_save_path={cache}"]),
        )
        .unwrap();
        assert_eq!(
            template.arguments(),
            args(&[
                "--model",
                "{model}",
                "--port",
                "{port}",
                "--slot-save-path",
                "{cache}",
                "--host",
                "127.0.0.1"
            ])
        );
        assert!(template.has_cache_binding());
        assert_eq!(template.clone().into_arguments(), template.arguments());
    }

    #[test]
    fn duplicate_owned_bindings_reject_even_when_values_match() {
        for duplicate in [
            args(&["-m", "{model}"]),
            args(&["--model={model}"]),
            args(&["--port", "{port}"]),
        ] {
            let mut input = base();
            input.extend(duplicate);
            assert_eq!(
                validate(Provider::LlamaCpp, &input).unwrap_err().code,
                "duplicate_runtime_binding"
            );
        }
        for duplicate in [
            args(&["--host", "127.0.0.1", "--host=127.0.0.1"]),
            args(&["--slot-save-path", "{cache}", "--slot_save_path", "{cache}"]),
        ] {
            let mut input = base();
            input.extend(duplicate);
            assert_eq!(
                validate(Provider::LlamaCpp, &input).unwrap_err().code,
                "duplicate_runtime_binding"
            );
        }
    }

    #[test]
    fn host_model_port_and_cache_values_cannot_override_manager_bindings() {
        for input in [
            args(&["--model", "other.gguf", "--port", "{port}"]),
            args(&["--model", "prefix{model}", "--port", "{port}"]),
            args(&["--model", "{model}", "--port", "1234"]),
            args(&[
                "--model", "{model}", "--port", "{port}", "--host", "0.0.0.0",
            ]),
            args(&[
                "--model",
                "{model}",
                "--port",
                "{port}",
                "--host",
                "127.0.0.1,0.0.0.0",
            ]),
            args(&[
                "--model",
                "{model}",
                "--port",
                "{port}",
                "--slot-save-path",
                "/unowned",
            ]),
        ] {
            assert_eq!(
                validate(Provider::LlamaCpp, &input).unwrap_err().code,
                "invalid_runtime_binding"
            );
        }
    }

    #[test]
    fn known_advanced_values_are_literal_and_not_scanned_as_flags_or_bindings() {
        let mut input = base();
        input.extend(args(&[
            "--chat-template",
            "--host",
            "--grammar",
            "--model",
            "--no-host",
            "--threads",
            "2",
        ]));
        let template = validate(Provider::LlamaCpp, &input).unwrap();
        assert_eq!(&template.arguments()[..input.len()], input);
        assert_eq!(
            &template.arguments()[input.len()..],
            args(&["--host", "127.0.0.1"])
        );
        let mut fake_cache = base();
        fake_cache.extend(args(&["--chat-template", "{cache}"]));
        assert!(
            !validate(Provider::LlamaCpp, &fake_cache)
                .unwrap()
                .has_cache_binding()
        );
    }

    #[test]
    fn typed_options_use_only_arity_owned_spans_and_preserve_original_positions() {
        let input = args(&[
            "--host",
            "127.0.0.1",
            "--model",
            "{model}",
            "--threads",
            "2",
            "--chat-template",
            "--ctx-size",
            "--ctx-size",
            "256",
            "--grammar",
            "--threads",
            "--port",
            "{port}",
            "--n-gpu-layers",
            "0",
        ]);
        let template = validate(Provider::LlamaCpp, &input).unwrap();
        let (unchanged, applied) = template.apply_options(&LaunchOptions::default()).unwrap();
        assert_eq!(unchanged.arguments(), input);
        assert_eq!(applied.context_tokens, Some(256));
        let (changed, applied) = template
            .apply_options(&LaunchOptions {
                context_tokens: Some(512),
                threads: Some(4),
                parallel_slots: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(applied.context_tokens, Some(512));
        assert_eq!(applied.threads, Some(4));
        assert_eq!(
            changed.arguments(),
            args(&[
                "--host",
                "127.0.0.1",
                "--model",
                "{model}",
                "--threads",
                "4",
                "--chat-template",
                "--ctx-size",
                "--ctx-size",
                "512",
                "--grammar",
                "--threads",
                "--port",
                "{port}",
                "--n-gpu-layers",
                "0",
                "--parallel",
                "1"
            ])
        );
    }

    #[test]
    fn extra_arguments_are_replaced_without_touching_bindings_typed_flags_or_kept_switches() {
        let template = validate(
            Provider::LlamaCpp,
            &args(&[
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--model",
                "{model}",
                "--slots",
                "--jinja",
                "-c",
                "4096",
                "--metrics",
                "--spec-type",
                "draft-mtp",
                "--chat-template-kwargs",
                "{\"enable_thinking\":false}",
            ]),
        )
        .unwrap();
        assert_eq!(
            template.extra_arguments(),
            args(&[
                "--jinja",
                "--spec-type",
                "draft-mtp",
                "--chat-template-kwargs",
                "{\"enable_thinking\":false}"
            ])
        );
        let next = template
            .with_extra_arguments(&args(&["--swa-full", "--spec-type=draft-mtp"]))
            .unwrap();
        assert_eq!(
            next.extra_arguments(),
            args(&["--swa-full", "--spec-type", "draft-mtp"])
        );
        for kept in ["--slots", "--metrics", "{model}", "{port}", "4096"] {
            assert!(
                next.arguments().iter().any(|argument| argument == kept),
                "{kept}"
            );
        }
        assert!(
            !next
                .arguments()
                .iter()
                .any(|argument| argument == "--jinja")
        );
        for (extras, code) in [
            (args(&["--temp", "0.5"]), "extra_argument_typed"),
            (args(&["--port", "1"]), "extra_argument_managed"),
            (args(&["--mmproj", "x.gguf"]), "extra_argument_managed"),
            (args(&["--no-slots"]), "extra_argument_managed"),
            (args(&["--lora", "x"]), "unsupported_runtime_argument"),
            (args(&["--made-up"]), "unsupported_runtime_argument"),
        ] {
            assert_eq!(
                template.with_extra_arguments(&extras).unwrap_err().code,
                code,
                "{extras:?}"
            );
        }
        assert_eq!(
            template
                .with_extra_arguments(&[])
                .unwrap()
                .extra_arguments(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn existing_option_duplicate_and_override_semantics_stay_with_the_shared_resolver() {
        let mut input = base();
        input.extend(args(&[
            "--threads",
            "2",
            "--chat-template",
            "literal",
            "-t",
            "2",
        ]));
        let template = validate(Provider::LlamaCpp, &input).unwrap();
        let (unchanged, _) = template.apply_options(&LaunchOptions::default()).unwrap();
        assert_eq!(unchanged.arguments(), template.arguments());
        let (changed, _) = template
            .apply_options(&LaunchOptions {
                threads: Some(3),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            changed.arguments(),
            args(&[
                "--model",
                "{model}",
                "--port",
                "{port}",
                "--threads",
                "3",
                "--chat-template",
                "literal",
                "--host",
                "127.0.0.1"
            ])
        );
        input.pop();
        input.push("4".into());
        assert_eq!(
            validate(Provider::LlamaCpp, &input)
                .unwrap()
                .apply_options(&LaunchOptions::default())
                .unwrap_err()
                .code,
            "conflicting_launch_arguments"
        );
    }

    #[test]
    fn unsafe_authority_download_companion_path_and_lifecycle_classes_reject() {
        for flag in [
            "--model_url",
            "-hf",
            "--docker-repo",
            "-hft",
            "-mmu",
            "--model-draft",
            "--lora",
            "--grammar-file",
            "--json-schema-file",
            "--rpc",
            "--connect",
            "--tools",
            "--mcp-servers-json",
            "--agent",
            "--api_key",
            "--ssl-key-file",
            "--api-prefix",
            "--models-dir",
            "--models-preset",
            "--path",
            "--media-path",
            "--log-file",
            "--log-prompts-dir",
            "--sleep-idle-seconds",
            "--fim-qwen-7b-spec",
        ] {
            let mut input = base();
            input.extend(args(&[flag, "private-value-never-echo"]));
            let error = validate(Provider::LlamaCpp, &input).unwrap_err();
            assert_eq!(error.code, "unbound_runtime_side_input", "{flag}");
            assert!(!error.to_string().contains("private-value"));
        }
    }

    #[test]
    fn llama_projectors_bind_only_as_retained_or_in_place_companions() {
        let mut input = base();
        input.extend(args(&["--mmproj", "/private/projector.gguf"]));
        let error = validate(Provider::LlamaCpp, &input).unwrap_err();
        assert_eq!(error.code, "invalid_runtime_binding");
        let mut input = base();
        input.extend(args(&[
            "--mmproj",
            "{member:mmproj}",
            "--no-mmproj-offload",
        ]));
        let template = validate(Provider::LlamaCpp, &input).unwrap();
        assert_eq!(
            template.member_requirements().collect::<Vec<_>>(),
            vec![("mmproj", ArtifactRole::Projector)]
        );
        assert!(template.verify_members([]).is_err());
        let resolved = template
            .resolve(Bindings {
                model: "/m.gguf",
                port: 9,
                cache: "/c",
                members: &[Member {
                    name: "mmproj",
                    role: ArtifactRole::Projector,
                    path: "/p/mmproj.gguf",
                }],
            })
            .unwrap();
        assert!(
            resolved
                .windows(2)
                .any(|pair| pair == ["--mmproj", "/p/mmproj.gguf"])
        );
        assert_eq!(
            passthrough(Provider::LlamaCpp, "--mmproj"),
            Passthrough::Companion
        );
        assert_eq!(
            passthrough(Provider::LlamaCpp, "--host"),
            Passthrough::ManagerOwned
        );
        assert_eq!(
            passthrough(Provider::LlamaCpp, "--lora"),
            Passthrough::Forbidden
        );
        assert_eq!(
            passthrough(Provider::LlamaCpp, "--spec-type"),
            Passthrough::Accepted(1)
        );
        assert_eq!(
            passthrough(Provider::LlamaCpp, "--jinja"),
            Passthrough::Accepted(0)
        );
        assert_eq!(
            passthrough(Provider::LlamaCpp, "--made-up"),
            Passthrough::Unknown
        );
        assert_eq!(
            passthrough(Provider::StableDiffusionCpp, "--params-backend"),
            Passthrough::Forbidden
        );
        assert_eq!(
            passthrough(Provider::StableDiffusionCpp, "--scheduler"),
            Passthrough::Accepted(1)
        );
    }

    #[test]
    fn missing_truncated_unknown_positional_and_delimiter_forms_reject() {
        for input in [
            args(&["--model", "{model}"]),
            args(&["--port", "{port}"]),
            args(&["--model"]),
        ] {
            assert!(validate(Provider::LlamaCpp, &input).is_err());
        }
        for tail in [
            args(&["--threads"]),
            args(&["--"]),
            args(&["positional"]),
            args(&["--new-upstream-flag", "value"]),
            args(&["--slots=true"]),
        ] {
            let mut input = base();
            input.extend(tail);
            assert!(validate(Provider::LlamaCpp, &input).is_err());
        }
    }

    #[test]
    fn whisper_requires_explicit_loopback_and_protects_its_selected_worker_contract() {
        assert_eq!(
            validate(Provider::WhisperCpp, &base()).unwrap_err().code,
            "missing_runtime_binding"
        );
        let mut input = base();
        input.extend(args(&[
            "--host",
            "127.0.0.1",
            "-p",
            "2",
            "--prompt",
            "--host",
            "--no-gpu",
        ]));
        assert!(
            !validate(Provider::WhisperCpp, &input)
                .unwrap()
                .has_cache_binding()
        );
        for flag in [
            "--translate",
            "-tr",
            "--public",
            "--request-path",
            "--inference-path",
            "--convert",
            "--tmp-dir",
            "--vad",
            "-vm",
            "--debug-mode",
        ] {
            let mut unsafe_input = input.clone();
            unsafe_input.push(flag.into());
            assert_eq!(
                validate(Provider::WhisperCpp, &unsafe_input)
                    .unwrap_err()
                    .code,
                "unbound_runtime_side_input"
            );
        }
        input.extend(args(&["--host", "0.0.0.0"]));
        assert_eq!(
            validate(Provider::WhisperCpp, &input).unwrap_err().code,
            "duplicate_runtime_binding"
        );
    }

    #[test]
    fn substitution_is_single_pass_literal_and_rejects_expansion_before_unbounded_allocation() {
        let template = validate(Provider::LlamaCpp, &base()).unwrap();
        let resolved = template
            .resolve(Bindings {
                model: "a {port} $() `literal` ✓",
                port: 1234,
                cache: "cache",
                members: &[],
            })
            .unwrap();
        assert_eq!(resolved[1], "a {port} $() `literal` ✓");
        assert_eq!(resolved[3], "1234");
        let mut input = base();
        input.extend(args(&["--chat-template", &"{model}".repeat(10000)]));
        assert!(validate(Provider::LlamaCpp, &input).is_err());
        input.pop();
        input.push("{model}".repeat(9000));
        let template = validate(Provider::LlamaCpp, &input).unwrap();
        let resolved = template
            .resolve(Bindings {
                model: "weights.gguf",
                port: 1,
                cache: "cache",
                members: &[],
            })
            .unwrap();
        assert_eq!(resolved[5], "{model}".repeat(9000));
        assert!(
            template
                .resolve(Bindings {
                    model: &"x".repeat(65537),
                    port: 1,
                    cache: "cache",
                    members: &[],
                })
                .is_err()
        );
        let generic = validate(Provider::ExternalHttp, &input).unwrap();
        assert!(
            generic
                .resolve(Bindings {
                    model: &"x".repeat(4096),
                    port: 1,
                    cache: "cache",
                    members: &[],
                })
                .is_err()
        );
    }

    #[test]
    fn selected_literal_templates_preserve_placeholder_text_after_typed_option_resolution() {
        let mut input = base();
        let literal = "{{ message }} {port} {model} {cache} --model --ctx-size";
        input.extend(args(&["--chat-template", literal, "--ctx-size", "128"]));
        let (template, _) = validate(Provider::LlamaCpp, &input)
            .unwrap()
            .apply_options(&LaunchOptions {
                context_tokens: Some(256),
                ..Default::default()
            })
            .unwrap();
        for port in [1234, 4321] {
            let resolved = template
                .resolve(Bindings {
                    model: "weights.gguf",
                    port,
                    cache: "/cache",
                    members: &[],
                })
                .unwrap();
            assert_eq!(resolved[1], "weights.gguf");
            assert_eq!(resolved[3], port.to_string());
            assert_eq!(resolved[5], literal);
            assert_eq!(&resolved[6..8], args(&["--ctx-size", "256"]));
        }
    }

    #[test]
    fn generic_http_templates_do_not_claim_selected_cache_or_argument_semantics() {
        let input = args(&[
            "--custom-port={port}",
            "--custom-model={model}",
            "--slot-save-path",
            "{cache}",
        ]);
        let template = validate(Provider::ExternalHttp, &input).unwrap();
        assert_eq!(template.arguments(), input);
        assert!(!template.has_cache_binding());
        assert!(template.apply_options(&LaunchOptions::default()).is_err());
        assert!(validate(Provider::LlamaCpp, &input).is_err());
    }

    #[test]
    fn manager_owned_environment_inputs_are_identified_without_reading_values() {
        for key in [
            "LLAMA_ARG_HOST",
            "LLAMA_ARG_NO_MODELS_AUTOLOAD",
            "LLAMA_API_KEY",
            "HF_TOKEN",
            "GGML_BACKEND_PATH",
            "WHISPER_ARG_DEVICE",
            "MODEL_ENDPOINT",
            "HF_ENDPOINT",
            "LLAMA_CACHE",
            "HF_HUB_CACHE",
            "HF_HOME",
            "HTTP_PROXY",
            "https_proxy",
            "no_proxy",
        ] {
            assert!(environment_is_reserved(key), "{key}");
        }
        for key in [
            "PATH",
            "HOME",
            "LANG",
            "OMP_NUM_THREADS",
            "LLAMA_NOT_A_REGISTERED_INPUT",
        ] {
            assert!(!environment_is_reserved(key));
        }
    }

    #[cfg(feature = "test-fixture")]
    #[test]
    fn physical_speech_gate_is_confined_to_the_explicit_fixture() {
        let input = args(&[
            "--host",
            "127.0.0.1",
            "--port",
            "{port}",
            "--model",
            "{model}",
            "--speech-physical-release-file",
            "owned.physical-release",
        ]);
        assert!(
            ArgumentTemplate::for_profile(
                Provider::WhisperCpp,
                "/owned/juiced-test-worker",
                &input
            )
            .is_ok()
        );
        assert!(
            ArgumentTemplate::for_profile(Provider::WhisperCpp, "whisper-server", &input).is_err()
        );
        assert!(validate(Provider::WhisperCpp, &input).is_err());
    }

    #[cfg(feature = "test-fixture")]
    #[test]
    fn fixture_extensions_need_the_exact_explicit_fixture_executable() {
        let mut input = base();
        input.extend(args(&[
            "--events",
            "owned.events",
            "--startup-release-file",
            "owned.release",
            "--parallel",
            "1",
            "--slot-save-path",
            "{cache}",
        ]));
        for command in ["/owned/juiced-test-worker", "juiced-test-worker.exe"] {
            let template =
                ArgumentTemplate::for_profile(Provider::ExternalHttp, command, &input).unwrap();
            assert!(template.has_cache_binding());
        }
        assert!(
            !ArgumentTemplate::for_profile(Provider::ExternalHttp, "other-worker", &input)
                .unwrap()
                .has_cache_binding()
        );
        for command in [
            "llama-server",
            "juiced-test-worker.backup",
            "not-juiced-test-worker",
        ] {
            assert!(ArgumentTemplate::for_profile(Provider::LlamaCpp, command, &input).is_err());
        }
        assert!(validate(Provider::LlamaCpp, &input).is_err());
    }

    #[cfg(not(feature = "test-fixture"))]
    #[test]
    fn production_builds_do_not_enable_fixture_extensions_by_executable_name() {
        let mut input = base();
        input.extend(args(&["--events", "owned.events"]));
        assert!(
            ArgumentTemplate::for_profile(Provider::LlamaCpp, "juiced-test-worker", &input)
                .is_err()
        );
    }

    #[test]
    fn stable_diffusion_binds_loopback_listener_model_and_role_checked_companions() {
        let template = validate(
            Provider::StableDiffusionCpp,
            &args(&[
                "--listen-port",
                "{port}",
                "--diffusion-model",
                "{model}",
                "--vae={member:ae.safetensors}",
                "--llm",
                "{member:qwen.gguf}",
                "--vae-tiling",
                "--steps",
                "8",
            ]),
        )
        .unwrap();
        assert_eq!(
            template.arguments(),
            args(&[
                "--listen-port",
                "{port}",
                "--diffusion-model",
                "{model}",
                "--vae",
                "{member:ae.safetensors}",
                "--llm",
                "{member:qwen.gguf}",
                "--vae-tiling",
                "--steps",
                "8",
                "--listen-ip",
                "127.0.0.1"
            ])
        );
        let members = [
            Member {
                name: "qwen.gguf",
                role: ArtifactRole::Encoder,
                path: "/store/qwen",
            },
            Member {
                name: "ae.safetensors",
                role: ArtifactRole::Vae,
                path: "/store/ae",
            },
        ];
        let resolved = template
            .resolve(Bindings {
                model: "/store/model",
                port: 4321,
                cache: "",
                members: &members,
            })
            .unwrap();
        assert_eq!(
            &resolved[..8],
            args(&[
                "--listen-port",
                "4321",
                "--diffusion-model",
                "/store/model",
                "--vae",
                "/store/ae",
                "--llm",
                "/store/qwen"
            ])
        );
        for wrong in [
            &members[..1],
            &[members[0], members[1], members[1]][..],
            &[
                Member {
                    role: ArtifactRole::Voice,
                    ..members[0]
                },
                members[1],
            ][..],
            &[
                members[0],
                members[1],
                Member {
                    name: "extra",
                    role: ArtifactRole::Vae,
                    path: "/store/extra",
                },
            ][..],
        ] {
            assert_eq!(
                template
                    .resolve(Bindings {
                        model: "/store/model",
                        port: 4321,
                        cache: "",
                        members: wrong,
                    })
                    .unwrap_err()
                    .code,
                "artifact_set_binding_mismatch"
            );
        }
        for unsafe_input in [
            args(&["--listen-port", "{port}", "-m", "{model}", "-l", "0.0.0.0"]),
            args(&["--listen-port", "{port}", "-m", "/etc/passwd"]),
            args(&[
                "--listen-port",
                "{port}",
                "-m",
                "{model}",
                "--vae",
                "/tmp/vae",
            ]),
            args(&[
                "--listen-port",
                "{port}",
                "-m",
                "{model}",
                "--vae",
                "{member:../x}",
            ]),
            args(&[
                "--listen-port",
                "{port}",
                "-m",
                "{model}",
                "--lora-model-dir",
                "/tmp",
            ]),
            args(&[
                "--listen-port",
                "{port}",
                "-m",
                "{model}",
                "--serve-html-path",
                "/x",
            ]),
            args(&[
                "--listen-port",
                "{port}",
                "-m",
                "{model}",
                "--upscale-model",
                "x",
            ]),
            args(&["--port", "{port}", "-m", "{model}"]),
            args(&[
                "--listen-port",
                "{port}",
                "-m",
                "{model}",
                "--vae",
                "{member:a}",
                "--vae",
                "{member:a}",
            ]),
        ] {
            assert!(
                validate(Provider::StableDiffusionCpp, &unsafe_input).is_err(),
                "{unsafe_input:?}"
            );
        }
    }

    #[test]
    fn kokoros_requires_ordered_server_subcommand_loopback_and_one_voice_pack() {
        let template = validate(
            Provider::Kokoros,
            &args(&[
                "-m",
                "{model}",
                "-d",
                "{member:voices-v1.0.bin}",
                "--instances",
                "1",
                "openai",
                "--port",
                "{port}",
            ]),
        )
        .unwrap();
        assert_eq!(
            template.arguments(),
            args(&[
                "--model",
                "{model}",
                "-d",
                "{member:voices-v1.0.bin}",
                "--instances",
                "1",
                "openai",
                "--port",
                "{port}",
                "--ip",
                "127.0.0.1"
            ])
        );
        assert_eq!(
            template.member_requirements().collect::<Vec<_>>(),
            [("voices-v1.0.bin", ArtifactRole::Voice)]
        );
        for invalid in [
            args(&["-m", "{model}", "openai", "--port", "{port}"]),
            args(&["-m", "{model}", "-d", "{member:v}", "--port", "{port}"]),
            args(&[
                "openai",
                "-m",
                "{model}",
                "-d",
                "{member:v}",
                "--port",
                "{port}",
            ]),
            args(&[
                "-m",
                "{model}",
                "-d",
                "{member:v}",
                "openai",
                "--port",
                "{port}",
                "--ip",
                "0.0.0.0",
            ]),
            args(&[
                "-m",
                "{model}",
                "-d",
                "{member:v}",
                "openai",
                "--port",
                "{port}",
                "--timestamps",
            ]),
            args(&[
                "-m",
                "{model}",
                "-d",
                "{member:v}",
                "openai",
                "openai",
                "--port",
                "{port}",
            ]),
            args(&[
                "-m",
                "{model}",
                "-d",
                "/tmp/voices",
                "openai",
                "--port",
                "{port}",
            ]),
            args(&[
                "-m",
                "{model}",
                "-d",
                "{member:v}",
                "text",
                "--port",
                "{port}",
            ]),
        ] {
            assert!(
                validate(Provider::Kokoros, &invalid).is_err(),
                "{invalid:?}"
            );
        }
    }
}
