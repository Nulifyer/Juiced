#![allow(dead_code)]

pub mod artifacts;
pub mod cli;
pub mod filesystem;
#[cfg(target_os = "linux")]
pub mod llama;
pub mod speech;

use reqwest::{Client, Method, Response, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

pub const PREFIXES: [&str; 4] = ["/api/v0", "/api/v1", "/v0", "/v1"];
pub const CLIENT_KEY: &str = "isolated-e2e-client-key";
pub const ADMIN_KEY: &str = "isolated-e2e-admin-key";
const ROTATED_CLIENT_KEY: &str = "isolated-e2e-rotated-client-key";
const ROTATED_ADMIN_KEY: &str = "isolated-e2e-rotated-admin-key";
pub const READ_ROUTES: [&str; 10] = [
    "/health",
    "/models",
    "/juiced/catalog",
    "/juiced/state",
    "/juiced/protocols",
    "/juiced/runtimes",
    "/juiced/runtimes/resolve",
    "/juiced/endpoints",
    "/models/{model}",
    "/models/{model}/options",
];
pub const OPERATOR_READ_ROUTES: [&str; 16] = [
    "/system-info",
    "/juiced/cache/inspect",
    "/juiced/requests",
    "/juiced/suitability",
    "/juiced/context",
    "/juiced/admission",
    "/juiced/cache",
    "/juiced/artifacts",
    "/juiced/runtime-packages",
    "/juiced/engines",
    "/juiced/artifacts/jobs/{job}",
    "/juiced/artifacts/sets/{set}",
    "/models/{model}/artifacts",
    "/models/{model}/runtime",
    "/system-stats",
    "/juiced/logs",
];
pub const WRITE_ROUTES: [&str; 22] = [
    "/juiced/runtime-packages/import",
    "/juiced/engines/install",
    "/juiced/models/add",
    "/juiced/policy",
    "/load",
    "/unload",
    "/juiced/admission",
    "/models/register",
    "/models/{model}/options",
    "/models/{model}/artifacts",
    "/models/{model}/runtime",
    "/delete",
    "/juiced/cache",
    "/juiced/cache/save",
    "/juiced/cache/restore",
    "/juiced/cache/forget",
    "/juiced/cache/recover",
    "/juiced/artifacts/pull",
    "/juiced/artifacts/jobs/{job}",
    "/juiced/artifacts/remove",
    "/juiced/artifacts/sets",
    "/juiced/artifacts/sets/remove",
];
pub const INFERENCE_ROUTES: [&str; 4] = [
    "/chat/completions",
    "/completions",
    "/responses",
    "/embeddings",
];
pub const MULTIPART_INFERENCE_ROUTES: [&str; 1] = ["/audio/transcriptions"];
pub const COMPAT_ROUTES: [(&str, &str, &str); 11] = [
    ("POST", "/v1/messages", "inference"),
    ("POST", "/v1/messages/count_tokens", "inference"),
    ("POST", "/api/chat", "inference"),
    ("POST", "/api/generate", "inference"),
    ("POST", "/api/embed", "inference"),
    ("POST", "/api/embeddings", "inference"),
    ("GET", "/api/tags", "read"),
    ("GET", "/api/ps", "read"),
    ("POST", "/api/show", "read"),
    ("GET", "/api/version", "read"),
    ("POST", "/mcp", "inference"),
];

pub fn route_matrix() -> BTreeSet<(String, String)> {
    READ_ROUTES
        .iter()
        .chain(OPERATOR_READ_ROUTES.iter())
        .map(|path| ("GET".into(), (*path).into()))
        .chain(
            WRITE_ROUTES
                .iter()
                .chain(INFERENCE_ROUTES.iter())
                .chain(MULTIPART_INFERENCE_ROUTES.iter())
                .map(|path| ("POST".into(), (*path).into())),
        )
        .chain(
            COMPAT_ROUTES
                .iter()
                .map(|(method, path, _)| ((*method).into(), (*path).into())),
        )
        .collect()
}

fn diagnostic_route(path: &str) -> &'static str {
    let path = path.split('?').next().unwrap_or("");
    for prefix in PREFIXES {
        let Some(path) = path.strip_prefix(prefix) else {
            continue;
        };
        if let Some(route) = READ_ROUTES
            .iter()
            .chain(OPERATOR_READ_ROUTES.iter())
            .chain(WRITE_ROUTES.iter())
            .chain(INFERENCE_ROUTES.iter())
            .chain(MULTIPART_INFERENCE_ROUTES.iter())
            .find(|route| **route == path)
        {
            return route;
        }
        if path
            .strip_prefix("/juiced/artifacts/jobs/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        {
            return "/juiced/artifacts/jobs/{job}";
        }
        if path
            .strip_prefix("/juiced/artifacts/sets/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        {
            return "/juiced/artifacts/sets/{set}";
        }
        if let Some(model) = path.strip_prefix("/models/") {
            if model
                .strip_suffix("/runtime")
                .is_some_and(|id| !id.is_empty() && !id.contains('/'))
            {
                return "/models/{model}/runtime";
            }
            if model
                .strip_suffix("/artifacts")
                .is_some_and(|id| !id.is_empty() && !id.contains('/'))
            {
                return "/models/{model}/artifacts";
            }
            if model
                .strip_suffix("/options")
                .is_some_and(|id| !id.is_empty() && !id.contains('/'))
            {
                return "/models/{model}/options";
            }
            if !model.is_empty() && !model.contains('/') {
                return "/models/{model}";
            }
        }
    }
    COMPAT_ROUTES
        .iter()
        .find(|(_, route, _)| *route == path)
        .map_or("<unregistered>", |(_, route, _)| *route)
}

pub fn binary(name: &str, override_name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(override_name) {
        let path = PathBuf::from(path);
        assert!(path.is_file(), "{override_name} does not name a file");
        return path;
    }
    let executable = std::env::current_exe().expect("locate integration-test executable");
    let directory = executable
        .parent()
        .and_then(Path::parent)
        .expect("Cargo test target directory");
    let path = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "Build service binaries once before running rust-tests: cargo build -p juiced-service --bins --features test-fixture. Alternatively set {override_name}."
    );
    path
}

pub fn metadata_gguf(training_context: u32) -> Vec<u8> {
    fn string(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend((text.len() as u64).to_le_bytes());
        bytes.extend(text.as_bytes());
    }
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(2u64.to_le_bytes());
    string(&mut bytes, "general.architecture");
    bytes.extend(8u32.to_le_bytes());
    string(&mut bytes, "llama");
    string(&mut bytes, "llama.context_length");
    bytes.extend(4u32.to_le_bytes());
    bytes.extend(training_context.to_le_bytes());
    bytes
}
pub fn canonical_model(mut model: Value) -> Value {
    let fields = model.as_object_mut().unwrap();
    let adapter = fields.remove("provider").unwrap_or(json!("external-http"));
    let implementation = match adapter.as_str().unwrap() {
        "external-http" => Value::Null,
        known => json!(known),
    };
    let bundle = fields.remove("runtime_bundle");
    let mut profile = json!({
        "schema_version":1,"adapter":adapter,"implementation_id":implementation,
        "command":fields.remove("command").unwrap(),"args":fields.remove("args").unwrap(),
        "options":fields.remove("options").unwrap_or(json!({})),"placement":"unresolved"
    });
    if let Some(bundle) = bundle {
        profile["runtime_bundle"] = bundle;
    }
    fields.insert("execution_profile".into(), profile);
    model
}
pub async fn rejected_manager(config: &Path) -> std::process::Output {
    fn capture(input: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            input.take(65536).read_to_end(&mut bytes).unwrap();
            bytes
        })
    }
    let mut command = Command::new(binary("lemond", "JUICED_TEST_SERVER"));
    command
        .arg("--config")
        .arg(config)
        .args(["--listen", "127.0.0.1:0"])
        .env_remove("LEMONADE_API_KEY")
        .env_remove("LEMONADE_ADMIN_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().unwrap();
    let stdout = capture(child.stdout.take().unwrap());
    let stderr = capture(child.stderr.take().unwrap());
    let mut process = Process {
        child,
        signal_group: true,
    };
    let status = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(status) = process.child.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("invalid configuration did not reject startup within three seconds");
    drop(process);
    let output = std::process::Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        !output.status.success(),
        "invalid configuration was accepted"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("listening on"),
        "configuration rejection occurred after opening the public listener"
    );
    output
}

struct Process {
    child: Child,
    signal_group: bool,
}

impl Process {
    fn terminate(&mut self) {
        if !self.signal_group {
            let _ = self.child.wait();
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &self.child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub struct Server {
    process: Option<Process>,
    pub directory: tempfile::TempDir,
    pub config_path: PathBuf,
    pub config: Value,
    pub base: String,
    pub client: Client,
    pub authenticated: bool,
    rotated_credentials: bool,
    isolated_loader_environment: bool,
    environment: BTreeMap<OsString, Option<OsString>>,
    working_directory: Option<PathBuf>,
}

impl Server {
    pub async fn new(authenticated: bool) -> Self {
        Self::configured(authenticated, |_| {}).await
    }

    pub async fn configured(authenticated: bool, edit: impl FnOnce(&mut Value)) -> Self {
        let mut result = Self::prepare(authenticated, edit);
        result.start().await;
        result
    }

    pub fn prepare(authenticated: bool, edit: impl FnOnce(&mut Value)) -> Self {
        let directory = tempfile::tempdir().expect("isolated test state");
        let weights = directory.path().join("fixture weights.gguf");
        std::fs::write(&weights, b"Synthetic fixture bytes, not model weights").unwrap();
        let worker = binary("juiced-test-worker", "JUICED_TEST_WORKER");
        let model = |id: &str, policy: &str| {
            json!({
                "id": id, "model_file": weights, "command": worker,
                "args": ["--port", "{port}", "--model", "{model}", "--events", directory.path().join(format!("{id}.events"))],
                "resident_bytes": 64, "max_inflight": 2,
                "operations": ["chat/completions", "completions", "responses", "embeddings", "messages/count_tokens"], "policy": policy,
            })
        };
        let mut config = json!({
            "schema_version":1, "inhibit_suspend":false, "resident_budget_bytes":128,
            "startup_timeout_ms":2000, "request_timeout_ms":3000,
            "models":[model("fixture.one", "on-demand"), model("fixture.two", "on-demand"), model("fixture.disabled", "disabled")],
        });
        edit(&mut config);
        let config_path = directory.path().join("config.json");
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        Self {
            process: None,
            directory,
            config_path,
            config,
            base: String::new(),
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()
                .unwrap(),
            authenticated,
            rotated_credentials: false,
            isolated_loader_environment: false,
            environment: BTreeMap::new(),
            working_directory: None,
        }
    }

    pub fn set_working_directory(&mut self, path: impl AsRef<Path>) {
        assert!(self.process.is_none());
        self.working_directory = Some(path.as_ref().to_owned());
    }

    pub fn set_environment(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        assert!(self.process.is_none());
        self.environment
            .insert(name.as_ref().to_owned(), Some(value.as_ref().to_owned()));
    }

    pub fn remove_environment(&mut self, name: impl AsRef<OsStr>) {
        assert!(self.process.is_none());
        self.environment.insert(name.as_ref().to_owned(), None);
    }

    pub fn isolate_loader_environment(&mut self) {
        assert!(self.process.is_none());
        self.isolated_loader_environment = true;
    }

    pub async fn start(&mut self) {
        self.start_mode(false).await;
    }

    pub async fn start_broadcast(&mut self) {
        self.start_mode(true).await;
    }

    async fn start_mode(&mut self, broadcast: bool) {
        assert!(self.process.is_none());
        let mut command = Command::new(binary("lemond", "JUICED_TEST_SERVER"));
        command
            .args(["--config"])
            .arg(&self.config_path)
            .args(["--listen", "127.0.0.1:0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("LEMONADE_API_KEY")
            .env_remove("LEMONADE_ADMIN_API_KEY");
        for (name, value) in &self.environment {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        if let Some(path) = &self.working_directory {
            command.current_dir(path);
        }
        if broadcast {
            command.arg("--broadcast");
        }
        if self.isolated_loader_environment {
            for name in [
                "LD_LIBRARY_PATH",
                "LD_PRELOAD",
                "LD_AUDIT",
                "DYLD_LIBRARY_PATH",
                "DYLD_INSERT_LIBRARIES",
            ] {
                command.env_remove(name);
            }
        }
        if self.authenticated {
            command
                .env("LEMONADE_API_KEY", self.read_key().unwrap())
                .env("LEMONADE_ADMIN_API_KEY", self.admin_key().unwrap());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().expect("start isolated lemond");
        let output = child.stdout.take().expect("read listener announcement");
        self.process = Some(Process {
            child,
            signal_group: true,
        });
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some(index) = line.find("http://127.0.0.1:") {
                    let address = line[index..].split_whitespace().next().unwrap().to_owned();
                    let _ = sender.send(address);
                }
            }
        });
        self.base =
            tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(10)))
                .await
                .unwrap()
                .expect("service did not announce an ephemeral listener");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self
                .request(Method::GET, "/v1/health", None, self.read_key())
                .await
            {
                Ok(response) if response.status().is_success() => break,
                _ if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                _ => panic!("isolated service did not become ready"),
            }
        }
    }

    pub fn stop(&mut self) {
        self.process.take();
    }

    pub fn crash(&mut self) {
        let mut process = self.process.take().expect("server is running");
        process
            .child
            .kill()
            .expect("terminate owned manager abruptly");
        process.child.wait().expect("reap crashed manager");
    }

    pub fn kill_manager_only(&mut self) {
        let process = self.process.as_mut().expect("server is running");
        process
            .child
            .kill()
            .expect("kill only the owned manager PID");
        process.child.wait().expect("reap owned manager");
    }

    pub async fn wait_for_manager_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(status) = self
                    .process
                    .as_mut()
                    .expect("manager process exists")
                    .child
                    .try_wait()
                    .unwrap()
                {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("owned manager did not exit within its shutdown deadline")
    }

    pub fn retire_reaped_manager_without_group_signal(&mut self) {
        let process = self.process.as_mut().expect("manager process exists");
        assert!(process.child.try_wait().unwrap().is_some());
        process.signal_group = false;
        self.process.take();
    }

    pub async fn restart(&mut self) {
        self.stop();
        self.start().await;
    }

    pub async fn rotate_credentials(&mut self) {
        self.stop();
        self.rotated_credentials = !self.rotated_credentials;
        self.start().await;
    }

    pub fn read_key(&self) -> Option<&'static str> {
        self.authenticated.then_some(if self.rotated_credentials {
            ROTATED_CLIENT_KEY
        } else {
            CLIENT_KEY
        })
    }

    pub fn pid(&self) -> u32 {
        self.process.as_ref().expect("server is running").child.id()
    }
    pub fn admin_key(&self) -> Option<&'static str> {
        self.authenticated.then_some(if self.rotated_credentials {
            ROTATED_ADMIN_KEY
        } else {
            ADMIN_KEY
        })
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Result<Response, reqwest::Error> {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await
    }

    pub async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let response = self
            .request(method.clone(), path, body, key)
            .await
            .expect("HTTP exchange");
        let status = response.status();
        let bytes = response.bytes().await.expect("complete response body");
        let value: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!(
                "expected JSON response with status {status}, got {} bytes",
                bytes.len()
            )
        });
        if !status.is_success() {
            let code = value["error"]["code"]
                .as_str()
                .filter(|code| {
                    !code.is_empty()
                        && code.len() <= 96
                        && code.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'_' | b'-')
                        })
                })
                .unwrap_or("<missing-or-invalid>");
            eprintln!(
                "HTTP failure method={} route={} status={} code={}",
                method.as_str(),
                diagnostic_route(path),
                status.as_u16(),
                code
            );
        }
        (status, value)
    }

    pub async fn get(&self, suffix: &str) -> Value {
        let path = format!("/v1/{suffix}");
        let operator = OPERATOR_READ_ROUTES.contains(&diagnostic_route(&path));
        let key = if operator {
            self.admin_key()
        } else {
            self.read_key()
        };
        let (status, value) = self
            .json(Method::GET, &format!("/v1/{suffix}"), None, key)
            .await;
        assert!(status.is_success(), "GET {suffix} returned {status}");
        value
    }

    pub async fn post(&self, suffix: &str, body: Value) -> (StatusCode, Value) {
        self.json(
            Method::POST,
            &format!("/v1/{suffix}"),
            Some(body),
            self.admin_key(),
        )
        .await
    }

    pub async fn model(&self, id: &str) -> Value {
        self.get("juiced/catalog").await["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == id)
            .expect("registered model")
            .clone()
    }

    pub async fn wait_for(&self, mut condition: impl AsyncFnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !condition().await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("condition did not become true within three seconds");
    }

    pub fn event(&self, model: &str, id: &str, event: &str) -> bool {
        self.event_count(model, id, event) > 0
    }

    pub fn event_count(&self, model: &str, id: &str, event: &str) -> usize {
        std::fs::read_to_string(self.directory.path().join(format!("{model}.events")))
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
            .filter(|line| {
                let value: Value = serde_json::from_str(line).expect("complete fixture event JSON");
                value["id"] == id && value["event"] == event
            })
            .count()
    }

    pub async fn socket_request(&self, suffix: &str, body: Value) -> tokio::net::TcpStream {
        let mut socket = tokio::net::TcpStream::connect(self.base.trim_start_matches("http://"))
            .await
            .unwrap();
        let body = body.to_string();
        let authorization = self
            .read_key()
            .map(|key| format!("Authorization: Bearer {key}\r\n"))
            .unwrap_or_default();
        socket.write_all(format!("POST /v1/{suffix} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{authorization}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        socket
    }
}
