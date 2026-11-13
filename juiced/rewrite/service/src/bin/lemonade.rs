use juiced_service::{
    artifacts::DownloadRequest, config::Registration, discovery, profiles::LaunchOptions,
};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Write},
    process::ExitCode,
    time::Duration,
};

const MAX_JSON: usize = 2 * 1024 * 1024;
const HELP: &str = r#"lemonade — client for the Juiced Rust service
Usage: lemonade [connection flags] COMMAND [command flags]
Connection: --url ORIGIN | --host HOST --port PORT (default 127.0.0.1:13306; 8080 in the container)
            --api-key KEY  --json  --timeout-ms MS  --discovery  --no-discovery
Environment: LEMONADE_ADMIN_API_KEY, then LEMONADE_API_KEY; explicit --api-key wins.
             LEMONADE_HOST and LEMONADE_PORT select an explicit target.
Explicit targets bypass discovery. Discovery accepts only local UDP senders.
Using credentials requires an explicit target; UDP beacons are unauthenticated.
Commands:
  list [--downloaded] [--filter TEXT]       All registered models
  models                                  Models exposed to clients
  status                                  Service state and reservations
  pause | resume                          Pause or resume new work; policies retained
  load MODEL [--ctx-size N] [--llamacpp-args ARGS] [--save-options] [--pin]
                                          Load a model; launch options are saved for it
  unload [MODEL]                          Unload one model, or every loaded model
  policy MODEL disabled|on-demand|hot --revision N
  register --file PATH|-                  Register a complete ModelSpec JSON; starts Disabled
  delete MODEL                            Unload, unregister and free unshared managed weights
  delete MODEL --revision N               Remove registration only; retain weight files
  pull MODEL [--checkpoint OWNER/REPO[:VARIANT]] [--recipe RECIPE]
                                          Download and register a catalog or Hugging Face model
  pull --file PATH|- --generation N       Submit DownloadRequest JSON, with bounded size + SHA256
  job [ID [pause|resume|retry|cancel|forget --revision N]]
  artifacts [remove SHA256 --generation N]
  cache [page flags]                      This credential's snapshot inventory
  cache enable|disable --generation N     Change persistent snapshot policy
  cache save --model MODEL --session ID --generation N --max-bytes N [--slot N]
  cache restore SNAPSHOT --model MODEL --session ID [--slot N]
  cache forget SNAPSHOT --session ID --revision N
  cache inspect [page flags]              Read-only file/checksum inspection
Page flags: --limit N --after ID --generation N --view TOKEN
            Inspection also accepts --orphan-after NAME. Copy generation/view from a prior page.
  context MODEL                           Declared, configured, applied and observed context
  options MODEL [--file PATH|- --revision N]  Read or change LaunchOptions JSON
  backends                                Configured model providers and physical slots
  backends install|uninstall RECIPE:BACKEND  Install or remove an engine build, e.g. llamacpp:cpu
  logs [--source SOURCE] [--limit N]      Recent service and worker log lines
  scan                                    Read-only native UDP beacon scan
  --help | --version
JSON files and '-' stdin are bounded to 2 MiB. Errors use stderr, JSON results stdout.
Cache saves require a prior completed session-bound /v1/completions request.
This client never starts or shuts down a daemon. It installs engines only on request.
Exit codes: 0 success, 1 local I/O, 2 usage, 3 connection/discovery, 4 authentication,
            5 service rejection, 6 incompatible or unreadable response.
"#;

#[derive(Debug)]
struct Failure {
    exit: u8,
    code: String,
    message: String,
    status: Option<u16>,
}
impl Failure {
    fn new(exit: u8, code: &str, message: &str) -> Self {
        Self {
            exit,
            code: code.to_owned(),
            message: message.to_owned(),
            status: None,
        }
    }
    fn usage(message: &str) -> Self {
        Self::new(2, "invalid_arguments", message)
    }
}
type Result<T> = std::result::Result<T, Failure>;
#[derive(Clone, Copy, Eq, PartialEq)]
enum Discovery {
    Auto,
    Force,
    Off,
}
struct Connection {
    origin: Url,
    explicit: bool,
    key: String,
    timeout: Duration,
    discovery: Discovery,
}
struct Invocation {
    connection: Connection,
    json: bool,
    command: Command,
}
enum Command {
    Help,
    Version,
    Scan,
    Backends,
    List {
        downloaded: bool,
        filter: Option<String>,
    },
    Call {
        path: String,
        body: Option<Value>,
    },
    Stream {
        path: String,
        body: Value,
    },
    Logs {
        source: Option<String>,
        limit: u64,
    },
    Input {
        kind: InputKind,
        file: String,
    },
}
enum InputKind {
    Register,
    Pull { generation: u64 },
    Options { model: String, revision: u64 },
}

fn origin(value: &str) -> Result<Url> {
    if value.chars().any(char::is_whitespace) || value.chars().any(char::is_control) {
        return Err(Failure::usage(
            "Use an HTTP or HTTPS origin without whitespace or control characters.",
        ));
    }
    let url = Url::parse(value).map_err(|_| {
        Failure::usage("Use an HTTP or HTTPS origin without credentials, path, query or fragment.")
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port_or_known_default() == Some(0)
    {
        return Err(Failure::usage(
            "Use an HTTP or HTTPS origin without credentials, path, query or fragment.",
        ));
    }
    Ok(url)
}
fn host_origin(host: &str, port: &str) -> Result<Url> {
    let port: u16 = port
        .parse()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| Failure::usage("Server port must be between 1 and 65535."))?;
    if host.is_empty()
        || host.chars().any(char::is_whitespace)
        || host.contains(['/', '\\', '?', '#', '@'])
    {
        return Err(Failure::usage(
            "Server host must be a hostname or IP address, without credentials or a path.",
        ));
    }
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    origin(&format!("http://{host}:{port}/"))
}
fn selected_key(
    explicit: Option<String>,
    admin: Option<String>,
    regular: Option<String>,
) -> String {
    explicit
        .or_else(|| admin.filter(|key| !key.is_empty()))
        .or_else(|| regular.filter(|key| !key.is_empty()))
        .unwrap_or_default()
}
fn value_after(args: &[String], index: &mut usize, inline: Option<&str>) -> Result<String> {
    if let Some(value) = inline {
        if !value.is_empty() {
            return Ok(value.to_owned());
        }
    } else {
        *index += 1;
        if let Some(value) = args.get(*index)
            && !value.starts_with("--")
        {
            return Ok(value.clone());
        }
    }
    Err(Failure::usage("An option is missing its required value."))
}
fn parse(args: Vec<String>, env: impl Fn(&str) -> Option<String>) -> Result<Invocation> {
    let mut host = env("LEMONADE_HOST")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    // The container image sets JUICED_DATA and serves on 8080.
    let mut port = env("LEMONADE_PORT")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if env("JUICED_DATA").is_some_and(|value| !value.is_empty()) {
                "8080".to_owned()
            } else {
                "13306".to_owned()
            }
        });
    let env_target = env("LEMONADE_HOST").is_some_and(|value| !value.is_empty())
        || env("LEMONADE_PORT").is_some_and(|value| !value.is_empty());
    let mut explicit_parts = false;
    let mut target = None;
    let mut key = None;
    let mut timeout = 30000;
    let mut discovery_mode = Discovery::Auto;
    let mut json_output = false;
    let mut command = Vec::new();
    let mut seen = BTreeSet::new();
    let mut index = 0;
    let mut positional_only = false;
    while index < args.len() {
        let raw = &args[index];
        if raw == "--" {
            positional_only = true;
            command.push(raw.clone());
            index += 1;
            continue;
        }
        let (flag, inline) = raw
            .split_once('=')
            .map_or((raw.as_str(), None), |(name, value)| (name, Some(value)));
        if !positional_only
            && matches!(
                flag,
                "--url"
                    | "--host"
                    | "--port"
                    | "--api-key"
                    | "--timeout-ms"
                    | "--json"
                    | "--discovery"
                    | "--no-discovery"
                    | "--help"
                    | "-h"
                    | "--version"
                    | "-v"
            )
        {
            if !seen.insert(flag.to_owned()) {
                return Err(Failure::usage(
                    "A connection or output option was specified more than once.",
                ));
            }
            match flag {
                "--url" => target = Some(value_after(&args, &mut index, inline)?),
                "--host" => {
                    host = value_after(&args, &mut index, inline)?;
                    explicit_parts = true;
                }
                "--port" => {
                    port = value_after(&args, &mut index, inline)?;
                    explicit_parts = true;
                }
                "--api-key" => key = Some(value_after(&args, &mut index, inline)?),
                "--timeout-ms" => {
                    timeout = value_after(&args, &mut index, inline)?
                        .parse::<u64>()
                        .ok()
                        .filter(|value| (1..=300000).contains(value))
                        .ok_or_else(|| {
                            Failure::usage("Timeout must be 1 to 300000 milliseconds.")
                        })?;
                }
                _ if inline.is_some() => {
                    return Err(Failure::usage("This flag does not accept a value."));
                }
                "--json" => json_output = true,
                "--discovery" => {
                    if discovery_mode == Discovery::Off {
                        return Err(Failure::usage(
                            "Choose either --discovery or --no-discovery.",
                        ));
                    }
                    discovery_mode = Discovery::Force;
                }
                "--no-discovery" => {
                    if discovery_mode == Discovery::Force {
                        return Err(Failure::usage(
                            "Choose either --discovery or --no-discovery.",
                        ));
                    }
                    discovery_mode = Discovery::Off;
                }
                "--help" | "-h" => {
                    return Ok(Invocation {
                        connection: Connection {
                            origin: origin("http://127.0.0.1:13306/")?,
                            explicit: true,
                            key: String::new(),
                            timeout: Duration::from_millis(timeout),
                            discovery: Discovery::Off,
                        },
                        json: json_output,
                        command: Command::Help,
                    });
                }
                "--version" | "-v" => {
                    return Ok(Invocation {
                        connection: Connection {
                            origin: origin("http://127.0.0.1:13306/")?,
                            explicit: true,
                            key: String::new(),
                            timeout: Duration::from_millis(timeout),
                            discovery: Discovery::Off,
                        },
                        json: json_output,
                        command: Command::Version,
                    });
                }
                _ => unreachable!(),
            }
        } else {
            command.push(raw.clone());
        }
        index += 1;
    }
    if target.is_some() && explicit_parts {
        return Err(Failure::usage(
            "Use --url or --host/--port, rather than both.",
        ));
    }
    let explicit = target.is_some() || explicit_parts || env_target;
    let origin = if let Some(value) = target {
        origin(&value)?
    } else {
        host_origin(&host, &port)?
    };
    let key = selected_key(key, env("LEMONADE_ADMIN_API_KEY"), env("LEMONADE_API_KEY"));
    let command = parse_command(command)?;
    if !key.is_empty()
        && !explicit
        && !matches!(command, Command::Scan | Command::Help | Command::Version)
    {
        return Err(Failure::usage(
            "Credentials require an explicit --url or --host/--port target. UDP discovery does not authenticate a service.",
        ));
    }
    if reqwest::header::HeaderValue::from_str(&key).is_err() {
        return Err(Failure::usage(
            "The selected API key is not a valid HTTP header value.",
        ));
    }
    Ok(Invocation {
        connection: Connection {
            origin,
            explicit,
            key,
            timeout: Duration::from_millis(timeout),
            discovery: discovery_mode,
        },
        json: json_output,
        command,
    })
}

struct Arguments {
    positional: Vec<String>,
    options: BTreeMap<String, String>,
    flags: BTreeSet<String>,
}
impl Arguments {
    fn parse(args: &[String], options: &[&str], flags: &[&str]) -> Result<Self> {
        let mut result = Self {
            positional: Vec::new(),
            options: BTreeMap::new(),
            flags: BTreeSet::new(),
        };
        let mut index = 0;
        let mut positional_only = false;
        while index < args.len() {
            let raw = &args[index];
            if raw == "--" {
                positional_only = true;
                index += 1;
                continue;
            }
            let (flag, inline) = raw
                .split_once('=')
                .map_or((raw.as_str(), None), |(name, value)| (name, Some(value)));
            if !positional_only && raw.starts_with('-') && raw != "-" {
                if options.contains(&flag) {
                    let value = value_after(args, &mut index, inline)?;
                    if result.options.insert(flag.to_owned(), value).is_some() {
                        return Err(Failure::usage(
                            "A command option was specified more than once.",
                        ));
                    }
                } else if flags.contains(&flag) && inline.is_none() {
                    if !result.flags.insert(flag.to_owned()) {
                        return Err(Failure::usage(
                            "A command flag was specified more than once.",
                        ));
                    }
                } else {
                    return Err(Failure::usage(
                        "Unknown or misplaced option for this command. See --help.",
                    ));
                }
            } else {
                result.positional.push(raw.clone());
            }
            index += 1;
        }
        Ok(result)
    }
    fn count(&self, count: usize) -> Result<()> {
        if self.positional.len() == count {
            Ok(())
        } else {
            Err(Failure::usage(
                "This command has missing or unexpected positional arguments. See --help.",
            ))
        }
    }
    fn required(&self, name: &str) -> Result<String> {
        self.options
            .get(name)
            .cloned()
            .ok_or_else(|| Failure::usage(&format!("This command requires {name}.")))
    }
    fn number(&self, name: &str) -> Result<u64> {
        self.required(name)?.parse().map_err(|_| {
            Failure::usage(
                "Revision, generation and size options require nonnegative whole numbers.",
            )
        })
    }
    fn model(&self, position: usize) -> Result<String> {
        bounded(
            self.positional
                .get(position)
                .ok_or_else(|| Failure::usage("This command requires a model ID."))?,
            256,
            "Model IDs must be nonempty and at most 256 bytes, without control characters.",
        )
    }
    fn session(&self) -> Result<String> {
        bounded(
            &self.required("--session")?,
            512,
            "Session identifiers must be nonempty and at most 512 bytes, without control characters.",
        )
    }
    fn slot(&self) -> Result<u32> {
        self.options
            .get("--slot")
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| Failure::usage("Slot must be a nonnegative 32-bit whole number."))
            })
            .unwrap_or(Ok(0))
    }
}
fn bounded(value: &str, limit: usize, message: &str) -> Result<String> {
    if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        Err(Failure::usage(message))
    } else {
        Ok(value.to_owned())
    }
}
fn component(value: &str) -> Result<String> {
    if matches!(value, "." | "..") {
        return Err(Failure::usage(
            "Dot-only IDs cannot be represented as API path segments.",
        ));
    }
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~' | b'.') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("string formatting");
        }
    }
    Ok(encoded)
}
fn call(path: &str, body: Option<Value>) -> Command {
    Command::Call {
        path: path.to_owned(),
        body,
    }
}
fn parse_command(mut args: Vec<String>) -> Result<Command> {
    if args.is_empty() {
        return Ok(Command::Help);
    }
    let command = args.remove(0);
    match command.as_str() {
        "list" => {
            let args = Arguments::parse(&args, &["--filter"], &["--downloaded"])?;
            args.count(0)?;
            Ok(Command::List {
                downloaded: args.flags.contains("--downloaded"),
                filter: args.options.get("--filter").cloned(),
            })
        }
        "backends" if !args.is_empty() => {
            let args = Arguments::parse(&args, &[], &[])?;
            args.count(2)?;
            let (recipe, backend) = args.positional[1]
                .split_once(':')
                .filter(|(recipe, backend)| !recipe.is_empty() && !backend.is_empty())
                .ok_or_else(|| {
                    Failure::usage("Name the engine as RECIPE:BACKEND, for example llamacpp:cpu.")
                })?;
            match args.positional[0].as_str() {
                "install" => Ok(Command::Stream {
                    path: "install".into(),
                    body: json!({"recipe":recipe,"backend":backend,"stream":true}),
                }),
                "uninstall" => Ok(call(
                    "uninstall",
                    Some(json!({"recipe":recipe,"backend":backend})),
                )),
                _ => Err(Failure::usage(
                    "Use backends install|uninstall RECIPE:BACKEND.",
                )),
            }
        }
        "logs" => {
            let args = Arguments::parse(&args, &["--source", "--limit"], &[])?;
            args.count(0)?;
            let source = args
                .options
                .get("--source")
                .map(|source| bounded(source, 256, "Log sources are at most 256 bytes."))
                .transpose()?;
            let limit = if args.options.contains_key("--limit") {
                args.number("--limit")?
            } else {
                50
            };
            Ok(Command::Logs { source, limit })
        }
        "models" | "status" | "pause" | "resume" | "backends" | "scan" => {
            Arguments::parse(&args, &[], &[])?.count(0)?;
            Ok(match command.as_str() {
                "models" => call("models", None),
                "status" => call("juiced/state", None),
                "pause" => call("juiced/admission", Some(json!({"paused":true}))),
                "resume" => call("juiced/admission", Some(json!({"paused":false}))),
                "backends" => Command::Backends,
                _ => Command::Scan,
            })
        }
        "unload" if args.is_empty() => Ok(call("unload", Some(json!({})))),
        "load" => {
            let args = Arguments::parse(
                &args,
                &["--ctx-size", "--llamacpp-args"],
                &["--save-options", "--pin"],
            )?;
            args.count(1)?;
            let model = args.model(0)?;
            if args.options.is_empty() && args.flags.is_empty() {
                return Ok(call("load", Some(json!({"model":model}))));
            }
            let mut body =
                json!({"model_name": model, "save_options": args.flags.contains("--save-options")});
            if args.options.contains_key("--ctx-size") {
                body["ctx_size"] = json!(args.number("--ctx-size")?);
            }
            if let Some(arguments) = args.options.get("--llamacpp-args") {
                body["llamacpp_args"] = json!(arguments);
            }
            if args.flags.contains("--pin") {
                body["pinned"] = json!(true);
            }
            Ok(call("load", Some(body)))
        }
        "unload" | "context" => {
            let args = Arguments::parse(&args, &[], &[])?;
            args.count(1)?;
            let model = args.model(0)?;
            if command == "context" {
                let mut url = Url::parse("http://localhost/juiced/context").expect("constant URL");
                url.query_pairs_mut().append_pair("model", &model);
                Ok(call(
                    &format!("juiced/context?{}", url.query().unwrap()),
                    None,
                ))
            } else {
                Ok(call(&command, Some(json!({"model":model}))))
            }
        }
        "policy" | "delete" => {
            let args = Arguments::parse(&args, &["--revision"], &[])?;
            args.count(if command == "policy" { 2 } else { 1 })?;
            let model = args.model(0)?;
            if command == "delete" && !args.options.contains_key("--revision") {
                return Ok(call("delete", Some(json!({"model_name":model}))));
            }
            let revision = args.number("--revision")?;
            if command == "delete" {
                Ok(call(
                    "delete",
                    Some(json!({"model":model,"expected_revision":revision})),
                ))
            } else {
                let policy = &args.positional[1];
                if !matches!(policy.as_str(), "disabled" | "on-demand" | "hot") {
                    return Err(Failure::usage("Policy must be disabled, on-demand or hot."));
                }
                Ok(call(
                    "juiced/policy",
                    Some(json!({"model":model,"policy":policy,"expected_revision":revision})),
                ))
            }
        }
        "pull" if args.first().is_some_and(|first| !first.starts_with('-')) => {
            let args = Arguments::parse(&args, &["--checkpoint", "--recipe"], &[])?;
            args.count(1)?;
            let mut model = args.model(0)?;
            if args.options.contains_key("--checkpoint") && !model.starts_with("user.") {
                model = format!("user.{model}");
            }
            let mut body = json!({"model_name": model, "stream": true});
            if let Some(checkpoint) = args.options.get("--checkpoint") {
                body["checkpoint"] = json!(checkpoint);
                body["recipe"] = json!(
                    args.options
                        .get("--recipe")
                        .map_or("llamacpp", String::as_str)
                );
            } else if args.options.contains_key("--recipe") {
                return Err(Failure::usage("--recipe requires --checkpoint."));
            }
            Ok(Command::Stream {
                path: "pull".into(),
                body,
            })
        }
        "register" | "pull" => {
            let args = Arguments::parse(
                &args,
                if command == "register" {
                    &["--file"]
                } else {
                    &["--file", "--generation"]
                },
                &[],
            )?;
            args.count(0)?;
            let file = args.required("--file")?;
            Ok(Command::Input {
                kind: if command == "register" {
                    InputKind::Register
                } else {
                    InputKind::Pull {
                        generation: args.number("--generation")?,
                    }
                },
                file,
            })
        }
        "options" => {
            let args = Arguments::parse(&args, &["--file", "--revision"], &[])?;
            args.count(1)?;
            let model = args.model(0)?;
            if let Some(file) = args.options.get("--file") {
                Ok(Command::Input {
                    kind: InputKind::Options {
                        model,
                        revision: args.number("--revision")?,
                    },
                    file: file.clone(),
                })
            } else if args.options.contains_key("--revision") {
                Err(Failure::usage(
                    "--revision requires an options JSON --file.",
                ))
            } else {
                Ok(call(
                    &format!("models/{}/options", component(&model)?),
                    None,
                ))
            }
        }
        "job" => {
            let args = Arguments::parse(&args, &["--revision"], &[])?;
            match args.positional.len() {
                0 if args.options.is_empty() => Ok(call("juiced/artifacts", None)),
                1 if args.options.is_empty() => Ok(call(
                    &format!("juiced/artifacts/jobs/{}", component(&args.positional[0])?),
                    None,
                )),
                2 if matches!(
                    args.positional[1].as_str(),
                    "pause" | "resume" | "retry" | "cancel" | "forget"
                ) =>
                {
                    Ok(call(
                        &format!("juiced/artifacts/jobs/{}", component(&args.positional[0])?),
                        Some(
                            json!({"action":args.positional[1],"expected_revision":args.number("--revision")?}),
                        ),
                    ))
                }
                _ => Err(Failure::usage(
                    "Use job [ID [pause|resume|retry|cancel|forget --revision N]].",
                )),
            }
        }
        "artifacts" => {
            let args = Arguments::parse(&args, &["--generation"], &[])?;
            if args.positional.is_empty() && args.options.is_empty() {
                Ok(call("juiced/artifacts", None))
            } else {
                args.count(2)?;
                if args.positional[0] != "remove" {
                    return Err(Failure::usage(
                        "Use artifacts [remove SHA256 --generation N].",
                    ));
                }
                Ok(call(
                    "juiced/artifacts/remove",
                    Some(
                        json!({"artifact":args.positional[1],"expected_generation":args.number("--generation")?}),
                    ),
                ))
            }
        }
        "cache" => cache_command(&args),
        _ => Err(Failure::usage(
            "Unknown command. See --help for implemented commands.",
        )),
    }
}
fn cache_command(raw: &[String]) -> Result<Command> {
    if raw.is_empty() || raw[0].starts_with('-') {
        return cache_page_command(raw, false);
    }
    let action = raw[0].as_str();
    let rest = &raw[1..];
    match action {
        "inspect" => cache_page_command(rest, true),
        "enable" | "disable" => {
            let args = Arguments::parse(rest, &["--generation"], &[])?;
            args.count(0)?;
            Ok(call(
                "juiced/cache",
                Some(
                    json!({"enabled":action == "enable","expected_generation":args.number("--generation")?}),
                ),
            ))
        }
        "save" => {
            let args = Arguments::parse(
                rest,
                &[
                    "--model",
                    "--session",
                    "--generation",
                    "--max-bytes",
                    "--slot",
                ],
                &[],
            )?;
            args.count(0)?;
            let maximum = args.number("--max-bytes")?;
            if maximum == 0 {
                return Err(Failure::usage("Snapshot --max-bytes must be positive."));
            }
            Ok(call(
                "juiced/cache/save",
                Some(
                    json!({"model":bounded(&args.required("--model")?,256,"Provide a bounded model ID.")?,"session":args.session()?,"slot":args.slot()?,"expected_generation":args.number("--generation")?,"maximum_bytes":maximum}),
                ),
            ))
        }
        "restore" => {
            let args = Arguments::parse(rest, &["--model", "--session", "--slot"], &[])?;
            args.count(1)?;
            Ok(call(
                "juiced/cache/restore",
                Some(
                    json!({"model":bounded(&args.required("--model")?,256,"Provide a bounded model ID.")?,"session":args.session()?,"slot":args.slot()?,"snapshot":args.positional[0]}),
                ),
            ))
        }
        "forget" => {
            let args = Arguments::parse(rest, &["--session", "--revision"], &[])?;
            args.count(1)?;
            Ok(call(
                "juiced/cache/forget",
                Some(
                    json!({"snapshot":args.positional[0],"session":args.session()?,"expected_revision":args.number("--revision")?}),
                ),
            ))
        }
        _ => Err(Failure::usage("Unknown cache action. See --help.")),
    }
}

fn cache_page_command(raw: &[String], inspection: bool) -> Result<Command> {
    let mut options = vec!["--limit", "--after", "--generation", "--view"];
    if inspection {
        options.push("--orphan-after");
    }
    let args = Arguments::parse(raw, &options, &[])?;
    args.count(0)?;
    let mut query = reqwest::Url::parse("http://localhost/").unwrap();
    for (option, key) in [
        ("--limit", "limit"),
        ("--after", "after"),
        ("--generation", "expected_generation"),
        ("--view", "expected_view"),
        ("--orphan-after", "orphan_after"),
    ] {
        if let Some(value) = args.options.get(option) {
            if matches!(option, "--limit" | "--generation") {
                args.number(option)?;
            }
            query.query_pairs_mut().append_pair(key, value);
        }
    }
    let path = if inspection {
        "juiced/cache/inspect"
    } else {
        "juiced/cache"
    };
    Ok(call(
        &query
            .query()
            .map_or_else(|| path.to_owned(), |query| format!("{path}?{query}")),
        None,
    ))
}

fn read_json(file: &str) -> Result<Vec<u8>> {
    let input: Box<dyn Read> = if file == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(file).map_err(|_| {
            Failure::new(
                1,
                "input_unavailable",
                "The JSON input file could not be opened.",
            )
        })?)
    };
    let mut bytes = Vec::new();
    input
        .take(MAX_JSON as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::new(1, "input_read", "The JSON input could not be read."))?;
    if bytes.len() > MAX_JSON {
        return Err(Failure::usage("JSON input exceeds the 2 MiB limit."));
    }
    serde_json::from_slice::<serde::de::IgnoredAny>(&bytes).map_err(|_| {
        Failure::usage("Input must be one valid JSON object, without trailing data.")
    })?;
    Ok(bytes)
}
fn input_call(kind: InputKind, value: &[u8]) -> Result<Command> {
    match kind {
        InputKind::Register => {
            let Registration(spec): Registration = serde_json::from_slice(value).map_err(|_| {
                Failure::usage("Register input must match the ModelSpec JSON schema.")
            })?;
            if spec.policy != juiced_service::config::Policy::Disabled || spec.revision != 0 {
                return Err(Failure::usage(
                    "New registrations must start Disabled at revision zero.",
                ));
            }
            Ok(call(
                "models/register",
                Some(serde_json::to_value(spec).expect("serializable model")),
            ))
        }
        InputKind::Pull { generation } => {
            let download: DownloadRequest = serde_json::from_slice(value).map_err(|_| {
                Failure::usage("Pull input must match the DownloadRequest JSON schema.")
            })?;
            Ok(call(
                "juiced/artifacts/pull",
                Some(json!({"download":download,"expected_generation":generation})),
            ))
        }
        InputKind::Options { model, revision } => {
            let options: LaunchOptions = serde_json::from_slice(value).map_err(|_| {
                Failure::usage("Options input must contain only typed LaunchOptions fields.")
            })?;
            options.validate().map_err(|_| {
                Failure::usage("Typed launch options are outside their supported bounds.")
            })?;
            Ok(call(
                &format!("models/{}/options", component(&model)?),
                Some(json!({"options":options,"expected_revision":revision})),
            ))
        }
    }
}

async fn request(
    client: &Client,
    connection: &Connection,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Value> {
    let url = connection
        .origin
        .join(&format!("v1/{path}"))
        .map_err(|_| Failure::usage("The API path could not be constructed."))?;
    let mut request = if let Some(body) = body {
        client.post(url).json(body)
    } else {
        client.get(url)
    };
    if !connection.key.is_empty() {
        request = request.bearer_auth(&connection.key);
    }
    let mut response = request.timeout(timeout).send().await.map_err(|_| {
        Failure::new(
            3,
            "connection_failed",
            if body.is_some() {
                "The service did not confirm this write. Inspect current state before retrying."
            } else {
                "The service request failed. Check the selected origin, credentials and connection."
            },
        )
    })?;
    let status = response.status();
    let mut bytes = Vec::new();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_JSON as u64)
    {
        return Err(Failure::new(
            6,
            "response_limit",
            "The service response exceeds the 2 MiB limit.",
        ));
    }
    while let Some(chunk) = response.chunk().await.map_err(|_| Failure::new(3,"response_interrupted","The service response was interrupted. Inspect committed state before retrying a write."))? {
        if chunk.len() > MAX_JSON.saturating_sub(bytes.len()) { return Err(Failure::new(6,"response_limit","The service response exceeds the 2 MiB limit.")); }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        let mut failure = Failure::new(
            if status.is_success() { 6 } else { 5 },
            "invalid_response",
            "The service returned an unreadable JSON response. Check the service version.",
        );
        failure.status = Some(status.as_u16());
        failure
    })?;
    if !status.is_success() {
        return Err(Failure {
            exit: if matches!(status.as_u16(), 401 | 403) {
                4
            } else {
                5
            },
            code: value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("service_rejected")
                .to_owned(),
            message: value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or(
                    "The service rejected the command. Refresh its current state before retrying.",
                )
                .to_owned(),
            status: Some(status.as_u16()),
        });
    }
    Ok(value)
}
/// Follows a native SSE progress stream to its `complete` or `error` event.
async fn stream(
    client: &Client,
    connection: &Connection,
    path: &str,
    body: &Value,
    quiet: bool,
) -> Result<Value> {
    let url = connection
        .origin
        .join(&format!("v1/{path}"))
        .map_err(|_| Failure::usage("The API path could not be constructed."))?;
    let mut request = client.post(url).json(body);
    if !connection.key.is_empty() {
        request = request.bearer_auth(&connection.key);
    }
    let interrupted = || {
        Failure::new(
            3,
            "stream_interrupted",
            "The progress stream ended early. The service keeps downloading; inspect GET /v1/downloads.",
        )
    };
    let mut response = request.send().await.map_err(|_| {
        Failure::new(
            3,
            "connection_failed",
            "The service did not confirm this request. Inspect current state before retrying.",
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        let bytes = response.bytes().await.unwrap_or_default();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        return Err(Failure {
            exit: if matches!(status.as_u16(), 401 | 403) {
                4
            } else {
                5
            },
            code: value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("service_rejected")
                .to_owned(),
            message: value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("The service rejected the command.")
                .to_owned(),
            status: Some(status.as_u16()),
        });
    }
    let mut buffer = Vec::new();
    let mut shown = String::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| interrupted())? {
        buffer.extend_from_slice(&chunk);
        if buffer.len() > MAX_JSON {
            return Err(Failure::new(
                6,
                "response_limit",
                "A progress event exceeds the 2 MiB limit.",
            ));
        }
        while let Some(end) = buffer.windows(2).position(|window| window == b"\n\n") {
            let message: Vec<u8> = buffer.drain(..end + 2).collect();
            let text = String::from_utf8_lossy(&message);
            let mut kind = "message";
            let mut data = Value::Null;
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    kind = match value.trim() {
                        "progress" => "progress",
                        "complete" => "complete",
                        "error" => "error",
                        _ => "message",
                    };
                } else if let Some(value) = line.strip_prefix("data:") {
                    data = serde_json::from_str(value.trim()).unwrap_or(Value::Null);
                }
            }
            match kind {
                "progress" if !quiet => {
                    let line = format!(
                        "[{}/{}] {} {}%",
                        data["file_index"],
                        data["total_files"],
                        data["file"]
                            .as_str()
                            .unwrap_or("")
                            .chars()
                            .filter(|ch| !ch.is_control())
                            .collect::<String>(),
                        data["percent"]
                    );
                    if line != shown {
                        let _ = writeln!(io::stderr().lock(), "{line}");
                        shown = line;
                    }
                }
                "complete" => {
                    let message = match (body["model_name"].as_str(), body["recipe"].as_str()) {
                        (Some(model), _) => format!("Installed model: {model}"),
                        (None, Some(recipe)) => format!(
                            "Installed backend: {recipe}:{}",
                            body["backend"].as_str().unwrap_or_default()
                        ),
                        _ => "Done".to_owned(),
                    };
                    let mut done = json!({"status": "success", "message": message});
                    for key in ["model_name", "recipe", "backend"] {
                        if body.get(key).is_some() {
                            done[key] = body[key].clone();
                        }
                    }
                    return Ok(done);
                }
                "error" => {
                    return Err(Failure {
                        exit: 5,
                        code: data["code"]
                            .as_str()
                            .unwrap_or("service_rejected")
                            .to_owned(),
                        message: data["error"]
                            .as_str()
                            .unwrap_or("The service reported a failure.")
                            .to_owned(),
                        status: Some(status.as_u16()),
                    });
                }
                _ => {}
            }
        }
    }
    Err(interrupted())
}
async fn state(client: &Client, connection: &Connection, timeout: Duration) -> Result<Value> {
    let value = request(client, connection, "juiced/state", None, timeout).await?;
    if value["schema_version"] != 1 || value["implementation"] != "rust-experiment" {
        return Err(Failure::new(
            6,
            "incompatible_service",
            "This CLI requires the matching Juiced Rust service. Select its origin explicitly with --url.",
        ));
    }
    Ok(value)
}
async fn connect(client: &Client, connection: &mut Connection) -> Result<Value> {
    let discover = !connection.explicit && connection.discovery != Discovery::Off;
    if connection.discovery != Discovery::Force || connection.explicit {
        match state(
            client,
            connection,
            if discover {
                connection.timeout.min(Duration::from_millis(300))
            } else {
                connection.timeout
            },
        )
        .await
        {
            Ok(value) => return Ok(value),
            Err(error) if !discover || error.exit != 3 => return Err(error),
            Err(_) => {}
        }
    }
    let candidates = discovery::discover_local(connection.timeout.min(Duration::from_secs(3))).await.map_err(|_| Failure::new(3,"discovery_unavailable","Local UDP discovery could not bind or receive. Select the service explicitly with --url."))?;
    for beacon in candidates.into_iter().take(16) {
        if let Some(origin) = beacon.origin() {
            connection.origin = origin;
            match state(
                client,
                connection,
                connection.timeout.min(Duration::from_millis(500)),
            )
            .await
            {
                Ok(value) => return Ok(value),
                Err(error) if error.exit == 4 => return Err(error),
                Err(_) => {}
            }
        }
    }
    Err(Failure::new(
        3,
        "service_not_found",
        "No matching local Rust service was discovered. Select its origin explicitly with --url.",
    ))
}
fn redact(value: &mut Value, key: &str) {
    if key.is_empty() {
        return;
    }
    match value {
        Value::String(value) if value.contains(key) => *value = value.replace(key, "[redacted]"),
        Value::Array(values) => values.iter_mut().for_each(|value| redact(value, key)),
        Value::Object(values) => {
            for value in values.values_mut() {
                redact(value, key);
            }
            if values.keys().any(|name| name.contains(key)) {
                *values = std::mem::take(values)
                    .into_iter()
                    .map(|(name, value)| (name.replace(key, "[redacted]"), value))
                    .collect();
            }
        }
        _ => {}
    }
}
fn human(value: &Value) -> String {
    if let Some(models) = value.get("data").and_then(Value::as_array)
        && models
            .iter()
            .all(|model| model.get("id").is_some() && model.get("policy").is_some())
    {
        let mut output = "MODEL\tPOLICY\tSTATE\tACTIVE\n".to_owned();
        for model in models {
            use std::fmt::Write;
            let label = |key: &str| {
                model[key]
                    .as_str()
                    .unwrap_or("unknown")
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect::<String>()
            };
            writeln!(
                output,
                "{}\t{}\t{}\t{}",
                label("id"),
                label("policy"),
                label("state"),
                model["active_requests"]
            )
            .expect("string formatting");
        }
        output.trim_end().to_owned()
    } else if let Some(lines) = value.get("data").and_then(Value::as_array)
        && value.get("next").is_some()
    {
        lines
            .iter()
            .map(|line| {
                format!(
                    "{} {}",
                    line["source"].as_str().unwrap_or("?"),
                    line["text"].as_str().unwrap_or("")
                )
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else if value["status"] == "success" && value["message"].is_string() {
        value["message"].as_str().unwrap_or_default().to_owned()
    } else if value["registration_removed"] == true && value.get("freed_bytes").is_some() {
        format!(
            "Deleted {}. Freed {} bytes{}.",
            value["model"],
            value["freed_bytes"],
            if value["weights_retained"] == true {
                "; shared or unmanaged weight files were kept"
            } else {
                ""
            }
        )
    } else if value["registration_removed"] == true {
        format!(
            "Removed registration for {}. Weight files retained.{}",
            value["model"],
            if value["artifact_reference_retained"] == true {
                " A storage reference remains retained; inspect artifacts before removing weights."
            } else {
                ""
            }
        )
    } else {
        serde_json::to_string_pretty(value).expect("JSON value serialization")
    }
}
fn output(mut value: Value, json_output: bool, key: &str, error: bool) -> io::Result<()> {
    redact(&mut value, key);
    let text = if json_output {
        serde_json::to_string(&value).expect("JSON value serialization")
    } else {
        human(&value)
    };
    if error {
        writeln!(io::stderr().lock(), "{text}")
    } else {
        writeln!(io::stdout().lock(), "{text}")
    }
}
async fn run(invocation: Invocation) -> Result<Value> {
    let Invocation {
        mut connection,
        command,
        json: json_output,
    } = invocation;
    let command = if let Command::Input { kind, file } = command {
        input_call(kind, &read_json(&file)?)?
    } else {
        command
    };
    if matches!(command, Command::Scan) {
        return discovery::scan(connection.timeout.min(Duration::from_secs(3)))
            .await
            .map(|data| json!({"data":data,"read_only":true}))
            .map_err(|_| {
                Failure::new(
                    3,
                    "discovery_unavailable",
                    "UDP scan could not bind or receive on port 13305.",
                )
            });
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(connection.timeout.min(Duration::from_secs(5)))
        .build()
        .map_err(|_| Failure::new(3, "client_setup", "The HTTP client could not initialize."))?;
    let state = connect(&client, &mut connection).await?;
    match command {
        Command::Call { path, body } if path == "juiced/state" && body.is_none() => Ok(state),
        Command::Call { path, body } => {
            request(
                &client,
                &connection,
                &path,
                body.as_ref(),
                connection.timeout,
            )
            .await
        }
        Command::Stream { path, body } => {
            stream(&client, &connection, &path, &body, json_output).await
        }
        Command::Logs { source, limit } => {
            let path = |after: u64, limit: u64| {
                let mut url = Url::parse("http://localhost/juiced/logs").expect("constant URL");
                {
                    let mut query = url.query_pairs_mut();
                    query.append_pair("after", &after.to_string());
                    query.append_pair("limit", &limit.to_string());
                    if let Some(source) = &source {
                        query.append_pair("source", source);
                    }
                }
                format!("juiced/logs?{}", url.query().unwrap_or_default())
            };
            let latest = request(&client, &connection, &path(0, 1), None, connection.timeout)
                .await?["latest"]
                .as_u64()
                .unwrap_or(0);
            request(
                &client,
                &connection,
                &path(latest.saturating_sub(limit), limit.max(1)),
                None,
                connection.timeout,
            )
            .await
        }
        Command::List { downloaded, filter } => {
            let mut value = request(
                &client,
                &connection,
                "juiced/catalog",
                None,
                connection.timeout,
            )
            .await?;
            let data = value
                .get_mut("data")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    Failure::new(
                        6,
                        "invalid_catalog",
                        "The service returned an invalid model catalog.",
                    )
                })?;
            data.retain(|model| {
                (!downloaded || model["downloaded"] == true)
                    && filter.as_ref().is_none_or(|filter| {
                        model["id"]
                            .as_str()
                            .is_some_and(|id| id.to_lowercase().contains(&filter.to_lowercase()))
                    })
            });
            Ok(value)
        }
        Command::Backends => {
            let catalog = request(
                &client,
                &connection,
                "juiced/catalog",
                None,
                connection.timeout,
            )
            .await?;
            let data = catalog["data"].as_array().ok_or_else(|| {
                Failure::new(
                    6,
                    "invalid_catalog",
                    "The service returned an invalid model catalog.",
                )
            })?;
            let mut providers = Vec::with_capacity(data.len());
            for model in data {
                let id = model["id"].as_str().ok_or_else(|| {
                    Failure::new(
                        6,
                        "invalid_catalog",
                        "The service returned an invalid model ID.",
                    )
                })?;
                providers.push(
                    request(
                        &client,
                        &connection,
                        &format!("models/{}/options", component(id)?),
                        None,
                        connection.timeout,
                    )
                    .await?,
                );
            }
            Ok(
                json!({"data":providers,"scope":"configured model providers","engine_installation":false}),
            )
        }
        _ => unreachable!("local commands handled before network"),
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_output = args.iter().any(|value| value == "--json");
    let invocation = match parse(args, |name| std::env::var(name).ok()) {
        Ok(value) => value,
        Err(error) => {
            let key = selected_key(
                None,
                std::env::var("LEMONADE_ADMIN_API_KEY").ok(),
                std::env::var("LEMONADE_API_KEY").ok(),
            );
            let _ = output(
                json!({"error":{"code":error.code,"message":error.message,"status":error.status}}),
                json_output,
                &key,
                true,
            );
            return ExitCode::from(error.exit);
        }
    };
    if matches!(invocation.command, Command::Help | Command::Version) {
        let result = if matches!(invocation.command, Command::Help) {
            writeln!(io::stdout().lock(), "{HELP}")
        } else {
            writeln!(
                io::stdout().lock(),
                "lemonade {} (Juiced Rust)",
                juiced_service::VERSION
            )
        };
        return ExitCode::from(if result.is_ok() { 0 } else { 1 });
    }
    let json_output = invocation.json;
    let key = invocation.connection.key.clone();
    match run(invocation).await {
        Ok(value) => ExitCode::from(if output(value, json_output, &key, false).is_ok() {
            0
        } else {
            1
        }),
        Err(error) => {
            let _ = output(
                json!({"error":{"code":error.code,"message":error.message,"status":error.status}}),
                json_output,
                &key,
                true,
            );
            ExitCode::from(error.exit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cli(args: &[&str]) -> Result<Invocation> {
        parse(
            args.iter().map(|value| (*value).to_owned()).collect(),
            |_| None,
        )
    }
    fn native(args: &[&str]) -> (String, Value) {
        match cli(args).unwrap().command {
            Command::Call {
                path,
                body: Some(body),
            }
            | Command::Stream { path, body } => (path, body),
            _ => panic!("expected a request with a body"),
        }
    }
    #[test]
    fn native_management_commands_build_v092_request_bodies() {
        assert_eq!(
            native(&["pull", "Tiny", "--checkpoint", "owner/repo:Q4_0"]),
            (
                "pull".into(),
                json!({"model_name":"user.Tiny","stream":true,"checkpoint":"owner/repo:Q4_0","recipe":"llamacpp"})
            )
        );
        assert_eq!(
            native(&["pull", "Qwen3-0.6B-GGUF"]),
            (
                "pull".into(),
                json!({"model_name":"Qwen3-0.6B-GGUF","stream":true})
            )
        );
        assert_eq!(
            native(&[
                "load",
                "one",
                "--ctx-size",
                "8192",
                "--llamacpp-args",
                "-np 2",
                "--pin"
            ]),
            (
                "load".into(),
                json!({"model_name":"one","save_options":false,"ctx_size":8192,"llamacpp_args":"-np 2","pinned":true})
            )
        );
        assert_eq!(
            native(&["load", "one"]),
            ("load".into(), json!({"model":"one"}))
        );
        assert_eq!(native(&["unload"]), ("unload".into(), json!({})));
        assert_eq!(
            native(&["delete", "one"]),
            ("delete".into(), json!({"model_name":"one"}))
        );
        assert_eq!(
            native(&["backends", "install", "llamacpp:cpu"]),
            (
                "install".into(),
                json!({"recipe":"llamacpp","backend":"cpu","stream":true})
            )
        );
        assert_eq!(
            native(&["backends", "uninstall", "whispercpp:vulkan"]),
            (
                "uninstall".into(),
                json!({"recipe":"whispercpp","backend":"vulkan"})
            )
        );
        let Command::Logs { source, limit } =
            cli(&["logs", "--source", "service", "--limit", "20"])
                .unwrap()
                .command
        else {
            panic!("logs reads recent lines");
        };
        assert_eq!((source.as_deref(), limit), (Some("service"), 20));
    }
    #[test]
    fn the_container_default_target_is_its_published_port() {
        let invocation = parse(vec!["status".into()], |name| {
            (name == "JUICED_DATA").then(|| "/data".to_owned())
        })
        .unwrap();
        assert_eq!(
            invocation.connection.origin.as_str(),
            "http://127.0.0.1:8080/"
        );
        assert!(!invocation.connection.explicit);
        assert_eq!(
            cli(&["status"]).unwrap().connection.origin.as_str(),
            "http://127.0.0.1:13306/"
        );
    }
    #[test]
    fn explicit_origins_reject_credentials_paths_queries_fragments_and_invalid_ports() {
        for value in [
            "file:///tmp",
            "http://u:secret@localhost/",
            "http://localhost/v1",
            "http://localhost/?key=secret",
            "http://localhost/#x",
            "http://localhost:0/",
        ] {
            assert!(origin(value).is_err());
        }
        assert_eq!(
            origin("https://localhost:13306").unwrap().as_str(),
            "https://localhost:13306/"
        );
        assert_eq!(
            host_origin("::1", "13306").unwrap().as_str(),
            "http://[::1]:13306/"
        );
        assert!(host_origin("localhost/private", "13306").is_err());
    }
    #[test]
    fn command_specific_options_reject_unknown_flags_and_missing_cas_values() {
        for args in [
            vec!["load", "one", "--generation", "0"],
            vec!["delete", "one", "--revision"],
            vec!["pull", "one", "--recipe", "llamacpp"],
            vec!["backends", "install", "llamacpp"],
            vec!["load", "one", "--ctx-size", "large"],
            vec!["policy", "one", "hot"],
            vec!["cache", "save", "--model", "one"],
            vec!["status", "--file", "-"],
            vec!["options", "one", "--revision", "3"],
            vec!["job", "id", "erase", "--revision", "1"],
            vec!["shutdown"],
            vec!["--url", "http://localhost/", "--port", "13306", "status"],
        ] {
            assert!(cli(&args).is_err());
        }
    }
    #[test]
    fn policy_and_job_controls_preserve_explicit_revisions() {
        let Command::Call {
            path,
            body: Some(body),
        } = cli(&["policy", "one", "hot", "--revision", "18446744073709551615"])
            .unwrap()
            .command
        else {
            panic!("expected mutation");
        };
        assert_eq!(path, "juiced/policy");
        assert_eq!(body["expected_revision"], u64::MAX);
        assert_eq!(body["policy"], "hot");
        let Command::Call {
            body: Some(body), ..
        } = cli(&["job", "opaque", "forget", "--revision", "5"])
            .unwrap()
            .command
        else {
            panic!("expected control");
        };
        assert_eq!(body, json!({"action":"forget","expected_revision":5}));
    }
    #[test]
    fn selected_targets_disable_discovery_and_explicit_credentials_override_environment() {
        let invocation = parse(
            vec![
                "--url=http://localhost:13306".to_owned(),
                "--api-key=explicit".to_owned(),
                "--discovery".to_owned(),
                "status".to_owned(),
            ],
            |name| match name {
                "LEMONADE_ADMIN_API_KEY" => Some("admin".to_owned()),
                "LEMONADE_API_KEY" => Some("regular".to_owned()),
                _ => None,
            },
        )
        .unwrap();
        assert!(invocation.connection.explicit);
        assert_eq!(invocation.connection.key, "explicit");
        assert_eq!(
            selected_key(None, Some("admin".to_owned()), Some("regular".to_owned())),
            "admin"
        );
        assert_eq!(
            selected_key(None, Some(String::new()), Some("regular".to_owned())),
            "regular"
        );
    }
    #[test]
    fn model_ids_are_encoded_as_one_segment_and_context_uses_an_encoded_query() {
        let Command::Call { path, .. } = cli(&["options", "org/model name"]).unwrap().command
        else {
            panic!("expected read")
        };
        assert_eq!(path, "models/org%2Fmodel%20name/options");
        let Command::Call { path, .. } = cli(&["context", "org/model?key=value"]).unwrap().command
        else {
            panic!("expected read")
        };
        assert_eq!(path, "juiced/context?model=org%2Fmodel%3Fkey%3Dvalue");
    }
    #[test]
    fn cache_session_and_size_are_bounded_without_persisting_or_guessing_identity() {
        let Command::Call {
            body: Some(body), ..
        } = cli(&[
            "cache",
            "save",
            "--model",
            "one",
            "--session",
            "opaque",
            "--generation",
            "3",
            "--max-bytes",
            "1024",
        ])
        .unwrap()
        .command
        else {
            panic!("expected cache request")
        };
        assert_eq!(
            body,
            json!({"model":"one","session":"opaque","slot":0,"expected_generation":3,"maximum_bytes":1024})
        );
        assert!(
            cli(&[
                "cache",
                "save",
                "--model",
                "one",
                "--session",
                "opaque",
                "--generation",
                "3",
                "--max-bytes",
                "0"
            ])
            .is_err()
        );
        assert!(
            cli(&[
                "cache",
                "forget",
                "snapshot",
                "--session",
                &"x".repeat(513),
                "--revision",
                "1"
            ])
            .is_err()
        );
    }
    #[test]
    fn cache_page_flags_preserve_tokens_and_encode_orphan_names_as_query_data() {
        let Command::Call { path, body } = cli(&[
            "cache",
            "inspect",
            "--limit",
            "16",
            "--after",
            "abc",
            "--generation",
            "9",
            "--view",
            "opaque",
            "--orphan-after",
            "file &name?x=y",
        ])
        .unwrap()
        .command
        else {
            panic!("expected cache page")
        };
        assert!(body.is_none());
        assert_eq!(
            path,
            "juiced/cache/inspect?limit=16&after=abc&expected_generation=9&expected_view=opaque&orphan_after=file+%26name%3Fx%3Dy"
        );
        let Command::Call { path, .. } = cli(&["cache", "--limit", "64"]).unwrap().command else {
            panic!("expected inventory page")
        };
        assert_eq!(path, "juiced/cache?limit=64");
        assert!(cli(&["cache", "--orphan-after", "file"]).is_err());
        assert!(cli(&["cache", "--limit", "many"]).is_err());
        assert!(cli(&["cache", "inspect", "--limit", "1", "--limit", "2"]).is_err());
    }
    #[test]
    fn typed_options_reject_unowned_fields_and_preserve_zero_gpu_layers() {
        assert!(
            input_call(
                InputKind::Options {
                    model: "one".to_owned(),
                    revision: 3
                },
                br#"{"command":"shell"}"#
            )
            .is_err()
        );
        let Command::Call {
            body: Some(body), ..
        } = input_call(
            InputKind::Options {
                model: "one".to_owned(),
                revision: 3,
            },
            br#"{"context_tokens":8192,"gpu_layers":0}"#,
        )
        .unwrap()
        else {
            panic!("expected options")
        };
        assert_eq!(body["options"]["gpu_layers"], 0);
        assert_eq!(body["options"]["threads"], Value::Null);
    }
    #[test]
    fn secrets_are_redacted_without_destroying_json_types_or_escaping() {
        let mut value = json!({"message":"private-key","private-key":{"value":"has private-key here"},"count":1});
        redact(&mut value, "private-key");
        assert_eq!(value["message"], "[redacted]");
        assert_eq!(value["count"], 1);
        assert!(
            !serde_json::to_string(&value)
                .unwrap()
                .contains("private-key")
        );
    }
    #[test]
    fn credentials_require_an_explicit_target_before_any_discovery_or_http() {
        let configured =
            |name: &str| (name == "LEMONADE_ADMIN_API_KEY").then(|| "private-key".to_owned());
        for args in [
            vec!["status"],
            vec!["--discovery", "status"],
            vec!["--no-discovery", "status"],
        ] {
            let result = parse(args.into_iter().map(str::to_owned).collect(), configured);
            let Err(error) = result else {
                panic!("credential-bearing implicit target was accepted")
            };
            assert_eq!(error.exit, 2);
            assert!(!error.message.contains("private-key"));
        }
        assert!(parse(vec!["scan".to_owned()], configured).is_ok());
        assert!(
            parse(
                vec![
                    "--host".to_owned(),
                    "127.0.0.1".to_owned(),
                    "status".to_owned()
                ],
                configured
            )
            .unwrap()
            .connection
            .explicit
        );
        let env_target = parse(vec!["status".to_owned()], |name| match name {
            "LEMONADE_ADMIN_API_KEY" => Some("private-key".to_owned()),
            "LEMONADE_PORT" => Some("13306".to_owned()),
            _ => None,
        })
        .unwrap();
        assert!(env_target.connection.explicit);
    }

    #[test]
    fn duplicate_input_fields_cannot_overwrite_registration_or_options_authority() {
        assert!(input_call(InputKind::Register, br#"{"id":"one","model_file":"weights","provider":"external-http","command":"worker","command":"other","args":["{model}","{port}"],"resident_bytes":1,"max_inflight":1,"operations":["completions"]}"#).is_err());
        assert!(
            input_call(
                InputKind::Options {
                    model: "one".into(),
                    revision: 0
                },
                br#"{"threads":1,"threads":2}"#
            )
            .is_err()
        );
    }

    #[test]
    fn explicit_json_files_are_size_bounded_and_reject_trailing_documents() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"{\"gpu_layers\":0}").unwrap();
        assert_eq!(
            read_json(file.path().to_str().unwrap()).unwrap(),
            br#"{"gpu_layers":0}"#
        );
        file.write_all(b" {} ").unwrap();
        assert!(read_json(file.path().to_str().unwrap()).is_err());
        file.as_file_mut().set_len((MAX_JSON + 1) as u64).unwrap();
        assert!(read_json(file.path().to_str().unwrap()).is_err());
    }
}
