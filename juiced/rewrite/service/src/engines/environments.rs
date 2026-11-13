//! Python engine environments.
//!
//! A Python engine such as vLLM is tens of thousands of files and several gigabytes, so it
//! cannot be a runtime package: package manifests list and hash every file on each launch.
//! An environment instead lives in its own store entry, `engine-environments/<id>/`, with
//! `environment.json` beside a read-only `root/`. The record carries a content digest of
//! every file computed at install time and a metadata stamp that launch re-checks cheaply.
//! The worker's executable mappings are still observed and hashed at startup like any
//! other engine.

use super::catalog::{Build, Engine, Environment, EnvironmentSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const STORE: &str = "engine-environments";
pub const CACHE: &str = "engine-cache";
const RECORD: &str = "environment.json";
const ROOT: &str = "root";
const MAX_RECORD_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 400_000;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_ENVIRONMENTS: usize = 16;
const COMMAND_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
const ROOT_PLACEHOLDER: &str = "{root}";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
    pub id: String,
    pub engine: Engine,
    pub backend: String,
    pub gpu_target: Option<String>,
    pub repository: String,
    pub tag: String,
    pub version: String,
    /// `Environment::pin` of the catalog entry it was installed from.
    pub pin: String,
    pub interpreter: PathBuf,
    pub library_directories: Vec<PathBuf>,
    /// Values may start with `{root}`, the environment root at launch.
    pub variables: BTreeMap<String, String>,
    pub files: u64,
    pub bytes: u64,
    pub content_sha256: String,
    pub stamp_sha256: String,
    pub licenses: Vec<String>,
    pub installed_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Storage,
    Layout,
    Changed,
    Cancelled,
    Install,
    Space,
    Limit,
}

impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Storage => "engine_storage",
            Self::Layout => "engine_environment_layout",
            Self::Changed => "engine_environment_changed",
            Self::Cancelled => "cancelled",
            Self::Install => "engine_environment_install",
            Self::Space => "engine_storage_space",
            Self::Limit => "engine_environment_limit",
        }
    }
}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Self::Storage
    }
}

pub fn store(config_directory: &Path) -> PathBuf {
    config_directory.join(STORE)
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("environment-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn read_record(entry: &Path) -> Result<Record, Error> {
    let file = fs::File::open(entry.join(RECORD))?;
    if file.metadata()?.len() > MAX_RECORD_BYTES {
        return Err(Error::Layout);
    }
    let record: Record = serde_json::from_reader(io::BufReader::new(file.take(MAX_RECORD_BYTES)))
        .map_err(|_| Error::Layout)?;
    if record.schema_version != 1
        || !valid_id(&record.id)
        || entry.file_name().and_then(|name| name.to_str()) != Some(record.id.as_str())
        || !relative(&record.interpreter)
        || !record.library_directories.iter().all(|path| relative(path))
    {
        return Err(Error::Layout);
    }
    Ok(record)
}

/// Installed environments, oldest first. Unreadable entries are left out.
pub fn list(store: &Path) -> Vec<Record> {
    let Ok(entries) = fs::read_dir(store) else {
        return Vec::new();
    };
    let mut records: Vec<Record> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| read_record(&entry.path()).ok())
        .take(MAX_ENVIRONMENTS)
        .collect();
    records.sort_by_key(|record| record.installed_unix_ms);
    records
}

pub fn entry(store: &Path, id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| store.join(id))
}

/// The interpreter path a launch profile names for an installed environment.
pub fn interpreter(store: &Path, record: &Record) -> PathBuf {
    store.join(&record.id).join(ROOT).join(&record.interpreter)
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// Digests of a tree: every file's content (only when `hash`) and every entry's metadata.
struct Manifest {
    files: u64,
    bytes: u64,
    content: String,
    stamp: String,
}

fn manifest(root: &Path, hash: bool, cancelled: &AtomicBool) -> Result<Manifest, Error> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let mut content = Sha256::new();
    content.update(b"juiced-engine-environment-content-v1\0");
    let mut stamp = Sha256::new();
    stamp.update(b"juiced-engine-environment-stamp-v1\0");
    let mut entries = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for item in fs::read_dir(root.join(&relative))? {
            let item = item?;
            let path = relative.join(item.file_name());
            entries.push(path.clone());
            if entries.len() > MAX_ENTRIES {
                return Err(Error::Limit);
            }
            if item.file_type()?.is_dir() {
                pending.push(path);
            }
        }
    }
    entries.sort();
    let (mut files, mut bytes) = (0u64, 0u64);
    for path in &entries {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let absolute = root.join(path);
        let metadata = fs::symlink_metadata(&absolute)?;
        let name = path.as_os_str().as_encoded_bytes();
        let kind: &[u8] = if metadata.is_dir() {
            b"d"
        } else if metadata.is_file() {
            b"f"
        } else if metadata.file_type().is_symlink() {
            b"l"
        } else {
            return Err(Error::Layout);
        };
        #[cfg(unix)]
        let (mode, modified, inode) = (
            metadata.mode(),
            metadata.mtime_nsec() as u64 + metadata.mtime() as u64 * 1_000_000_000,
            metadata.ino(),
        );
        #[cfg(not(unix))]
        let (mode, modified, inode) = (0u32, 0u64, 0u64);
        for part in [
            name,
            kind,
            &metadata.len().to_le_bytes(),
            &mode.to_le_bytes(),
            &modified.to_le_bytes(),
            &inode.to_le_bytes(),
        ] {
            stamp.update((part.len() as u64).to_le_bytes());
            stamp.update(part);
        }
        if !hash {
            continue;
        }
        content.update((name.len() as u64).to_le_bytes());
        content.update(name);
        content.update(kind);
        if metadata.is_file() {
            files += 1;
            bytes = bytes
                .checked_add(metadata.len())
                .filter(|bytes| *bytes <= MAX_TOTAL_BYTES)
                .ok_or(Error::Limit)?;
            let digest =
                super::archive::file_sha256(&absolute, cancelled).map_err(|error| match error {
                    super::archive::ArchiveError::Cancelled => Error::Cancelled,
                    _ => Error::Storage,
                })?;
            content.update(digest.as_bytes());
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&absolute)?;
            if !link_inside(path, &target) {
                return Err(Error::Layout);
            }
            content.update(target.as_os_str().as_encoded_bytes());
        }
    }
    Ok(Manifest {
        files,
        bytes,
        content: format!("{:x}", content.finalize()),
        stamp: format!("{:x}", stamp.finalize()),
    })
}

/// A relative link that resolves inside the environment root.
fn link_inside(link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = link.components().count().saturating_sub(1) as isize;
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

pub struct Verified {
    pub record: Record,
    pub root: PathBuf,
    pub entry: PathBuf,
}

/// The environment whose root holds `executable`, when it is one.
pub fn locate(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|ancestor| {
            ancestor.file_name().is_some_and(|name| name == ROOT)
                && ancestor
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == STORE)
        })
        .and_then(Path::parent)
        .map(Path::to_owned)
}

/// Confirms the environment is exactly as installed by comparing its metadata stamp.
pub fn verify(entry: &Path) -> Result<Verified, Error> {
    let record = read_record(entry)?;
    let root = entry.join(ROOT);
    let current = manifest(&root, false, &AtomicBool::new(false))?;
    if current.stamp != record.stamp_sha256 {
        return Err(Error::Changed);
    }
    Ok(Verified {
        record,
        root,
        entry: entry.to_owned(),
    })
}

fn resolve(root: &Path, value: &str) -> String {
    match value.strip_prefix(ROOT_PLACEHOLDER) {
        Some(rest) => format!("{}{rest}", root.display()),
        None => value.to_owned(),
    }
}

/// `libcuda.so.1` from the driver the container runtime injected. Triton links its
/// launcher stubs with `-lcuda`, which needs the unversioned name the driver does not ship.
fn driver_library() -> Option<PathBuf> {
    [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/usr/local/nvidia/lib64",
        "/usr/lib/wsl/lib",
    ]
    .iter()
    .map(|directory| Path::new(directory).join("libcuda.so.1"))
    .find(|path| path.is_file())
}

/// The worker environment for a verified environment: its library path, its variables, a
/// writable compile cache outside the read-only root, and no network model downloads.
pub fn launch(
    verified: &Verified,
    cache: &Path,
    workspace: &Path,
) -> Result<Vec<(String, String)>, Error> {
    for directory in ["vllm", "triton", "inductor", "xdg", "home"] {
        fs::create_dir_all(cache.join(directory))?;
    }
    let mut variables: Vec<(String, String)> = verified
        .record
        .variables
        .iter()
        .map(|(key, value)| (key.clone(), resolve(&verified.root, value)))
        .collect();
    if !verified.record.library_directories.is_empty() {
        let paths: Vec<PathBuf> = verified
            .record
            .library_directories
            .iter()
            .map(|path| verified.root.join(path))
            .collect();
        let joined = std::env::join_paths(paths).map_err(|_| Error::Layout)?;
        variables.push((
            "LD_LIBRARY_PATH".into(),
            joined.to_string_lossy().into_owned(),
        ));
    }
    let directory = |name: &str| cache.join(name).to_string_lossy().into_owned();
    variables.extend([
        ("HOME".into(), directory("home")),
        ("XDG_CACHE_HOME".into(), directory("xdg")),
        ("VLLM_CACHE_ROOT".into(), directory("vllm")),
        ("TRITON_CACHE_DIR".into(), directory("triton")),
        ("TORCHINDUCTOR_CACHE_DIR".into(), directory("inductor")),
        ("HF_HUB_OFFLINE".into(), "1".into()),
        ("TRANSFORMERS_OFFLINE".into(), "1".into()),
        ("VLLM_NO_USAGE_STATS".into(), "1".into()),
        ("DO_NOT_TRACK".into(), "1".into()),
    ]);
    if verified.record.backend == "cuda"
        && let Some(driver) = driver_library()
    {
        let links = workspace.join("driver");
        fs::create_dir_all(&links)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&driver, links.join("libcuda.so"))?;
        variables.push((
            "TRITON_LIBCUDA_PATH".into(),
            links.to_string_lossy().into_owned(),
        ));
    }
    Ok(variables)
}

/// Verifies the environment holding `executable` and returns its worker variables, with the
/// compile cache in `engine-cache/<id>` beside the store.
pub fn launch_for(executable: &Path, workspace: &Path) -> Result<Vec<(String, String)>, Error> {
    let entry = locate(executable).ok_or(Error::Layout)?;
    let verified = verify(&entry)?;
    if entry.join(ROOT).join(&verified.record.interpreter) != executable
        && fs::canonicalize(entry.join(ROOT).join(&verified.record.interpreter))? != executable
    {
        return Err(Error::Layout);
    }
    let cache = entry
        .parent()
        .and_then(Path::parent)
        .ok_or(Error::Layout)?
        .join(CACHE)
        .join(&verified.record.id);
    launch(&verified, &cache, workspace)
}

/// Variables a worker environment sets itself; inherited values are dropped so the
/// service's own settings never steer the engine.
pub fn reserved_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("VLLM_")
        || name.starts_with("PYTHON")
        || name.starts_with("TORCH")
        || name.starts_with("TRITON_")
        || matches!(
            name.as_str(),
            "CC" | "CXX" | "VIRTUAL_ENV" | "CUDA_HOME" | "ROCM_PATH" | "HIP_PATH"
        )
}

pub fn remove(entry: &Path) -> io::Result<()> {
    fn writable(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
            for item in fs::read_dir(path)? {
                writable(&item?.path())?;
            }
        }
        Ok(())
    }
    match writable(entry) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    }
    fs::remove_dir_all(entry)
}

pub struct InstallRequest<'a> {
    pub build: Build,
    pub environment: &'static Environment,
    pub staging: &'a Path,
    pub store: &'a Path,
    /// The downloaded, digest-verified archives in catalog order.
    pub archives: Vec<PathBuf>,
    pub cancelled: &'a AtomicBool,
    pub log: &'a (dyn Fn(String) + Sync),
}

/// Bytes free on the filesystem holding `path`.
pub fn available_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `name` is a valid C string and `stats` is a writable statvfs.
        (unsafe { libc::statvfs(name.as_ptr(), &mut stats) } == 0)
            .then(|| stats.f_bavail as u64 * stats.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Builds the environment under `staging`, seals it read-only and moves it into the store.
pub fn install(request: InstallRequest<'_>) -> Result<Record, Error> {
    let tree = request.staging.join("environment");
    let root = tree.join(ROOT);
    fs::create_dir_all(&root)?;
    let (interpreter, library_directories, mut variables) = match &request.environment.source {
        EnvironmentSource::Uv {
            python,
            torch_backend,
            lock,
            ..
        } => uv(&request, &root, python, torch_backend, lock)?,
        EnvironmentSource::Parts { .. } => parts(&request, &root)?,
    };
    variables.retain(|key, _| !key.is_empty());
    seal(&root)?;
    let sealed = manifest(&root, true, request.cancelled)?;
    let mut identity = Sha256::new();
    identity.update(b"juiced-engine-environment-v1\0");
    for part in [
        request.build.engine.name(),
        request.build.backend,
        &request.environment.pin(),
        &sealed.content,
    ] {
        identity.update((part.len() as u64).to_le_bytes());
        identity.update(part.as_bytes());
    }
    let id = format!("environment-{:x}", identity.finalize());
    let record = Record {
        schema_version: 1,
        id: id.clone(),
        engine: request.build.engine,
        backend: request.build.backend.to_owned(),
        gpu_target: request.build.gpu_target.map(str::to_owned),
        repository: request.build.repository.to_owned(),
        tag: request.build.tag.to_owned(),
        version: request.environment.version.to_owned(),
        pin: request.environment.pin(),
        interpreter,
        library_directories,
        variables,
        files: sealed.files,
        bytes: sealed.bytes,
        content_sha256: sealed.content,
        stamp_sha256: sealed.stamp,
        licenses: super::catalog::licenses(&request.build),
        installed_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_millis() as u64),
    };
    let mut bytes = serde_json::to_vec_pretty(&record).map_err(|_| Error::Storage)?;
    bytes.push(b'\n');
    let mut file = fs::File::create(tree.join(RECORD))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::create_dir_all(request.store)?;
    let destination = request.store.join(&id);
    if destination.exists() {
        remove(&destination)?;
    }
    fs::rename(&tree, &destination)?;
    #[cfg(unix)]
    if let Ok(directory) = fs::File::open(request.store) {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor is open for the duration of the call.
        unsafe { libc::syncfs(directory.as_raw_fd()) };
    }
    Ok(record)
}

fn run(
    request: &InstallRequest<'_>,
    program: &Path,
    arguments: &[&str],
    environment: &[(&str, &Path)],
) -> Result<(), Error> {
    let log_path = request.staging.join("command.log");
    let log = fs::File::create(&log_path)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("UV_NO_CONFIG", "1")
        .env("UV_NO_PROGRESS", "1")
        .env("UV_PYTHON_DOWNLOADS", "automatic")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .current_dir(request.staging);
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    let mut child = command.spawn().map_err(|_| Error::Install)?;
    let deadline = Instant::now() + COMMAND_DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if request.cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    if !status.success() {
        let mut tail = String::new();
        if let Ok(mut file) = fs::File::open(&log_path) {
            let length = file.metadata().map_or(0, |metadata| metadata.len());
            let _ = io::Seek::seek(&mut file, io::SeekFrom::Start(length.saturating_sub(2048)));
            let _ = file.read_to_string(&mut tail);
        }
        (request.log)(format!(
            "Engine environment command {} failed ({status}): {}",
            program.display(),
            tail.trim()
        ));
        return Err(Error::Install);
    }
    Ok(())
}

type Layout = (PathBuf, Vec<PathBuf>, BTreeMap<String, String>);

/// The Triton launcher stubs need a C compiler and the image ships none, so the
/// environment carries `ziglang` and this wrapper is its `CC`.
const ZIG_CC: &str =
    "#!/bin/sh\nexec \"${0%/*}/../lib/python3.12/site-packages/ziglang/zig\" cc \"$@\"\n";

fn uv(
    request: &InstallRequest<'_>,
    root: &Path,
    python: &str,
    torch_backend: &str,
    lock: &str,
) -> Result<Layout, Error> {
    let tools = request.staging.join("uv");
    let [archive] = request.archives.as_slice() else {
        return Err(Error::Layout);
    };
    fs::create_dir_all(&tools)?;
    super::archive::extract(
        archive,
        request.build.format,
        &tools,
        &super::archive::LIMITS,
        request.cancelled,
    )
    .map_err(|error| {
        (request.log)(format!(
            "The uv archive could not be extracted: {}",
            error.code()
        ));
        Error::Layout
    })?;
    let uv = find(&tools, "uv").ok_or(Error::Layout)?;
    let cache = request.staging.join("uv-cache");
    let home = request.staging.join("home");
    let pythons = request.staging.join("pythons");
    fs::create_dir_all(&home)?;
    let environment = [("UV_CACHE_DIR", cache.as_path()), ("HOME", home.as_path())];
    (request.log)(format!(
        "Installing CPython {python} for {}",
        request.environment.version
    ));
    run(
        request,
        &uv,
        &[
            "python",
            "install",
            python,
            "--install-dir",
            &pythons.to_string_lossy(),
            "--no-bin",
            "--no-registry",
        ],
        &environment,
    )?;
    let installed = fs::read_dir(&pythons)
        .inspect_err(|_| (request.log)(format!("uv installed no CPython {python}")))?
        .flatten()
        .find(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("cpython-{python}-"))
        })
        .ok_or(Error::Layout)?;
    let prefix = root.join("python");
    fs::rename(installed.path(), &prefix)?;
    let minor = python.rsplit_once('.').map_or(python, |(minor, _)| minor);
    let interpreter = PathBuf::from("python/bin").join(format!("python{minor}"));
    let requirements = request.staging.join("requirements.txt");
    fs::write(&requirements, lock)?;
    (request.log)(format!("Installing {} wheels", request.environment.version));
    run(
        request,
        &uv,
        &[
            "pip",
            "install",
            "--python",
            &root.join(&interpreter).to_string_lossy(),
            "--break-system-packages",
            "--require-hashes",
            "--no-deps",
            "--compile-bytecode",
            "--torch-backend",
            torch_backend,
            "-r",
            &requirements.to_string_lossy(),
        ],
        &environment,
    )?;
    let compiler = prefix.join("bin/juiced-cc");
    fs::write(&compiler, ZIG_CC)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&compiler, fs::Permissions::from_mode(0o755))?;
    }
    for stale in ["bin/pip", "bin/pip3", "bin/idle3", "bin/2to3", "bin/pydoc3"] {
        let _ = fs::remove_file(prefix.join(stale));
    }
    let _ = fs::remove_file(prefix.join(format!("bin/pip{minor}")));
    let mut variables = BTreeMap::new();
    variables.insert(
        "CC".into(),
        format!("{ROOT_PLACEHOLDER}/python/bin/juiced-cc"),
    );
    variables.insert("VLLM_USE_FLASHINFER_SAMPLER".into(), "0".into());
    Ok((interpreter, Vec::new(), variables))
}

fn find(root: &Path, name: &str) -> Option<PathBuf> {
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(directory).ok()?.flatten() {
            let path = item.path();
            if item.file_type().ok()?.is_dir() {
                pending.push(path);
            } else if item.file_name() == name {
                return Some(path);
            }
        }
    }
    None
}

/// The ROCm environment: the parts concatenate into one `.tar.gz` of a relocatable
/// CPython prefix with vLLM, PyTorch, the ROCm SDK wheels and their clang.
fn parts(request: &InstallRequest<'_>, root: &Path) -> Result<Layout, Error> {
    let readers = request
        .archives
        .iter()
        .map(fs::File::open)
        .collect::<io::Result<Vec<_>>>()?;
    let chained = readers
        .into_iter()
        .fold(Box::new(io::empty()) as Box<dyn Read>, |chain, file| {
            Box::new(chain.chain(io::BufReader::with_capacity(1 << 20, file)))
        });
    (request.log)(format!("Extracting {}", request.environment.version));
    untar(
        flate2::read::GzDecoder::new(chained),
        root,
        request.cancelled,
    )?;
    for archive in &request.archives {
        let _ = fs::remove_file(archive);
    }
    let site = Path::new("lib/python3.12/site-packages");
    if !root.join("bin/python3").is_file() || !root.join(site).join("vllm").is_dir() {
        return Err(Error::Layout);
    }
    let mut library_directories = Vec::new();
    for entry in fs::read_dir(root.join(site))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("_rocm_sdk_") && entry.path().join("lib").is_dir() {
            library_directories.push(site.join(&name).join("lib"));
        }
    }
    library_directories.sort();
    for extra in [
        site.join("torch/lib"),
        site.join("_rocm_sdk_core/lib/llvm/lib"),
        "lib".into(),
    ] {
        if root.join(&extra).is_dir() {
            library_directories.push(extra);
        }
    }
    // The archive stores its executables without execute permission.
    mark_executables(root)?;
    // `-I` ignores PYTHONPATH, so the startup additions native ran through a wrapper
    // script are a `.pth` file the site module still reads.
    fs::write(root.join(site).join("juiced_rocm_startup.py"), ROCM_STARTUP)?;
    fs::write(
        root.join(site).join("juiced-rocm.pth"),
        "_rocm_sdk_core/share/amd_smi\nimport juiced_rocm_startup\n",
    )?;
    let mut variables = BTreeMap::new();
    variables.insert("FLASH_ATTENTION_TRITON_AMD_ENABLE".into(), "TRUE".into());
    let clang = site.join("_rocm_sdk_core/lib/llvm/bin/clang");
    if root.join(&clang).is_file() {
        variables.insert(
            "CC".into(),
            format!("{ROOT_PLACEHOLDER}/{}", clang.display()),
        );
    }
    Ok(("bin/python3".into(), library_directories, variables))
}

/// The native ROCm startup shim: vLLM probes NVML before ROCm and the probe fails on hosts
/// without the NVIDIA driver.
const ROCM_STARTUP: &str = "import sys\n\ntry:\n    import vllm.utils.import_utils as _vllm_import_utils\n\n    def _disabled_import_pynvml():\n        raise ModuleNotFoundError(\"vLLM NVML probe disabled by Juiced for the ROCm backend\")\n\n    _vllm_import_utils.import_pynvml = _disabled_import_pynvml\nexcept (ImportError, AttributeError) as exc:\n    print(f\"Juiced warning: unable to disable vLLM NVML probe: {exc}\", file=sys.stderr)\n";

fn mark_executables(root: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            for item in fs::read_dir(&directory)?.flatten() {
                let kind = item.file_type()?;
                let path = item.path();
                if kind.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                let mut magic = [0u8; 4];
                let elf = fs::File::open(&path)
                    .and_then(|mut file| file.read_exact(&mut magic))
                    .is_ok()
                    && magic == *b"\x7fELF";
                let script = directory.file_name().is_some_and(|name| name == "bin")
                    && magic.starts_with(b"#!");
                if elf || script {
                    let mode = item.metadata()?.permissions().mode();
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o111))?;
                }
            }
        }
    }
    Ok(())
}

/// Extracts a tar stream into `root`. Paths and links stay inside the root; nothing is
/// fsynced per file because the store entry is synced once before it is published.
fn untar(reader: impl Read, root: &Path, cancelled: &AtomicBool) -> Result<(), Error> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = 0usize;
    let mut bytes = 0u64;
    for entry in archive.entries().map_err(|_| Error::Layout)? {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(Error::Limit);
        }
        let mut entry = entry.map_err(|_| Error::Layout)?;
        let raw = entry.path().map_err(|_| Error::Layout)?.into_owned();
        let Some(relative) =
            super::archive::normalize(&raw.to_string_lossy()).map_err(|_| Error::Layout)?
        else {
            continue;
        };
        let path = root.join(&relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.header().entry_type() {
            tar::EntryType::Directory => fs::create_dir_all(&path)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                bytes = bytes
                    .checked_add(entry.size())
                    .filter(|bytes| *bytes <= MAX_TOTAL_BYTES)
                    .ok_or(Error::Limit)?;
                let mut file = fs::File::create(&path)?;
                io::copy(&mut entry, &mut file).map_err(|_| Error::Layout)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let executable = entry.header().mode().is_ok_and(|mode| mode & 0o111 != 0);
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
                    )?;
                }
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(|_| Error::Layout)?
                    .ok_or(Error::Layout)?
                    .into_owned();
                if !link_inside(&relative, &target) {
                    return Err(Error::Layout);
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &path)?;
            }
            tar::EntryType::Link => {
                let target = entry
                    .link_name()
                    .map_err(|_| Error::Layout)?
                    .ok_or(Error::Layout)?;
                let target = super::archive::normalize(&target.to_string_lossy())
                    .map_err(|_| Error::Layout)?
                    .ok_or(Error::Layout)?;
                fs::hard_link(root.join(target), &path)?;
            }
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader => {}
            _ => return Err(Error::Layout),
        }
    }
    Ok(())
}

/// Makes the environment read-only, so neither the engine nor an operator shell can change
/// it in place without the stamp noticing.
fn seal(root: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut directories = Vec::new();
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            for item in fs::read_dir(&directory)?.flatten() {
                let kind = item.file_type()?;
                if kind.is_dir() {
                    pending.push(item.path());
                } else if kind.is_file() {
                    let mode = item.metadata()?.permissions().mode();
                    fs::set_permissions(item.path(), fs::Permissions::from_mode(mode & 0o555))?;
                }
            }
            directories.push(directory);
        }
        for directory in directories.iter().rev() {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o555))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(root: &Path) -> Record {
        let sealed = manifest(root, true, &AtomicBool::new(false)).unwrap();
        Record {
            schema_version: 1,
            id: format!("environment-{}", "a".repeat(64)),
            engine: Engine::Vllm,
            backend: "cuda".into(),
            gpu_target: None,
            repository: "astral-sh/uv".into(),
            tag: "0.13.7".into(),
            version: "vllm test".into(),
            pin: "uv:test".into(),
            interpreter: "python/bin/python3.12".into(),
            library_directories: vec!["lib".into()],
            variables: BTreeMap::from([("CC".into(), "{root}/python/bin/juiced-cc".into())]),
            files: sealed.files,
            bytes: sealed.bytes,
            content_sha256: sealed.content,
            stamp_sha256: sealed.stamp,
            licenses: vec!["Apache-2.0".into()],
            installed_unix_ms: 1,
        }
    }

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let entry = base
            .path()
            .join(STORE)
            .join(format!("environment-{}", "a".repeat(64)));
        let root = entry.join(ROOT);
        fs::create_dir_all(root.join("python/bin")).unwrap();
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(root.join("python/bin/python3.12"), b"\x7fELF interpreter").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("python3.12", root.join("python/bin/python3")).unwrap();
        (base, entry)
    }

    #[test]
    fn sealed_environments_verify_and_detect_changes() {
        let (_base, entry) = tree();
        let root = entry.join(ROOT);
        seal(&root).unwrap();
        let record = record(&root);
        fs::write(entry.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
        let verified = verify(&entry).unwrap();
        assert_eq!(verified.record, record);
        assert_eq!(
            locate(&root.join("python/bin/python3.12")).as_deref(),
            Some(entry.as_path())
        );
        assert_eq!(list(entry.parent().unwrap()), vec![record.clone()]);
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let variables: BTreeMap<_, _> = launch(&verified, cache.path(), workspace.path())
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            variables["CC"],
            format!("{}/python/bin/juiced-cc", root.display())
        );
        assert_eq!(
            variables["LD_LIBRARY_PATH"],
            root.join("lib").to_string_lossy()
        );
        assert_eq!(variables["HF_HUB_OFFLINE"], "1");
        assert!(variables["VLLM_CACHE_ROOT"].starts_with(&*cache.path().to_string_lossy()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root.join("lib"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(root.join("lib/extra.so"), b"planted").unwrap();
            assert_eq!(verify(&entry).err(), Some(Error::Changed));
        }
        remove(&entry).unwrap();
        assert!(!entry.exists());
    }

    #[test]
    fn links_must_stay_inside_the_root() {
        assert!(link_inside(
            Path::new("bin/python3"),
            Path::new("python3.12")
        ));
        assert!(link_inside(Path::new("a/b/c"), Path::new("../../d")));
        assert!(!link_inside(Path::new("a/b"), Path::new("../../d")));
        assert!(!link_inside(Path::new("a"), Path::new("/usr/lib/libc.so")));
    }

    #[test]
    fn split_archives_extract_as_one_stream_and_reject_escapes() {
        let source = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, "./bin/python3", &b"\x7fELF!"[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "./bin/python", "python3")
            .unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let (first, second) = bytes.split_at(bytes.len() / 2);
        let parts = [source.path().join("part1"), source.path().join("part2")];
        fs::write(&parts[0], first).unwrap();
        fs::write(&parts[1], second).unwrap();
        let readers: Vec<Box<dyn Read>> = parts
            .iter()
            .map(|path| Box::new(fs::File::open(path).unwrap()) as Box<dyn Read>)
            .collect();
        let chained = readers
            .into_iter()
            .fold(Box::new(io::empty()) as Box<dyn Read>, |chain, next| {
                Box::new(chain.chain(next))
            });
        let root = tempfile::tempdir().unwrap();
        untar(
            flate2::read::GzDecoder::new(chained),
            root.path(),
            &AtomicBool::new(false),
        )
        .unwrap();
        mark_executables(root.path()).unwrap();
        assert_eq!(
            fs::read(root.path().join("bin/python")).unwrap(),
            b"\x7fELF!"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.path().join("bin/python3"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "escape", "../../etc/passwd")
            .unwrap();
        let escaping = builder.into_inner().unwrap();
        let target = tempfile::tempdir().unwrap();
        assert_eq!(
            untar(&escaping[..], target.path(), &AtomicBool::new(false)).err(),
            Some(Error::Layout)
        );
    }
}
