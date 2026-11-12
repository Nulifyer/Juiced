use crate::config::Provider;
use std::{collections::VecDeque, fmt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    fn new(code: &'static str, message: &'static str) -> Self {
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentTemplate {
    arguments: Vec<String>,
    cache_binding: bool,
    adapter: Provider,
    fixture: bool,
    typed_spans: Vec<(usize, usize)>,
    bindings: [Option<usize>; 4],
}

impl ArgumentTemplate {
    pub fn for_profile(
        adapter: Provider,
        command: &str,
        arguments: &[String],
    ) -> Result<Self, Error> {
        if fixture_command(command) {
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
        let mut fields: [VecDeque<usize>; 4] = std::array::from_fn(|_| VecDeque::new());
        for group in resolved.args.chunks_exact(2) {
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
                arguments.extend_from_slice(group);
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
        let port = bindings.port.to_string();
        let mut arguments = Vec::with_capacity(self.arguments.len());
        let mut bytes = 0usize;
        for (position, argument) in self.arguments.iter().enumerate() {
            let resolved = if self.adapter == Provider::ExternalHttp && !self.fixture {
                substitute(argument, &port, bindings.model, bindings.cache)?
            } else {
                let value = if self.bindings[0] == Some(position) {
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
            bindings: [None; 4],
        });
    }
    validate_selected(adapter, arguments, false)
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

fn owned(flag: &str, adapter: Provider, fixture: bool) -> Option<(usize, &'static str)> {
    match flag {
        "-m" | "--model" => Some((0, "--model")),
        "--port" => Some((1, "--port")),
        "--host" => Some((2, "--host")),
        "--slot-save-path" if adapter == Provider::LlamaCpp || fixture => {
            Some((3, "--slot-save-path"))
        }
        _ => None,
    }
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
    let mut position = 0;
    while let Some(argument) = arguments.get(position) {
        let (raw_flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        let flag = normalized_flag(adapter, raw_flag);
        if let Some((index, canonical)) = owned(&flag, adapter, fixture) {
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
        let arity = if fixture { fixture_arity(&flag) } else { None }
            .or_else(|| match adapter {
                Provider::LlamaCpp => llama_arity(&flag),
                Provider::WhisperCpp => whisper_arity(&flag),
                Provider::ExternalHttp => None,
            })
            .ok_or_else(|| {
                Error::new(
                    "unsupported_runtime_argument",
                    "The selected adapter does not define this worker argument or positional form.",
                )
            })?;
        if adapter == Provider::LlamaCpp
            && let Some(field) = crate::profiles::option_field(&flag)
        {
            typed_spans.push((result.len(), field));
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
    if !seen[2] {
        result.extend(["--host".into(), "127.0.0.1".into()]);
    }
    bounded(&result)?;
    Ok(ArgumentTemplate {
        arguments: result,
        cache_binding: seen[3],
        adapter,
        fixture,
        typed_spans,
        bindings,
    })
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
        Provider::ExternalHttp => false,
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
            | "--speech-response-mode"
            | "--speech-release-file"
            | "--speech-physical-release-file"
            | "--speech-status"
            | "--startup-log"
            | "--chatty-bytes" => Some(1),
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
            "--mmproj",
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
            })
            .unwrap();
        assert_eq!(resolved[5], "{model}".repeat(9000));
        assert!(
            template
                .resolve(Bindings {
                    model: &"x".repeat(65537),
                    port: 1,
                    cache: "cache"
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
}
