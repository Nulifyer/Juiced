use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod backends;
mod command;
mod sysfs;
#[cfg(test)]
mod tests;

pub use backends::{BackendVariant, Inventory, Reason, ReasonCode, project};
pub use command::run_bounded;

pub const CACHE_TTL: Duration = Duration::from_secs(15);
pub const DETECTION_BUDGET: Duration = Duration::from_secs(12);
const RESULT_GRACE: Duration = Duration::from_secs(2);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const NVIDIA_SMI_OUTPUT_BYTES: usize = 64 * 1024;
const VULKANINFO_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Output(String),
    NotFound,
    TimedOut,
    Failed(Option<i32>),
    OutputLimit,
    InvalidOutput,
    SpawnFailed,
}

impl CommandOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Output(_) => "ok",
            Self::NotFound => "not_found",
            Self::TimedOut => "timed_out",
            Self::Failed(_) => "failed",
            Self::OutputLimit => "output_limit",
            Self::InvalidOutput => "invalid_output",
            Self::SpawnFailed => "spawn_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceAccess {
    ReadWrite,
    Denied,
    Missing,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Unavailable,
    Unknown,
    Available,
}

/// Every filesystem read is relative to `root`, so fixture trees can stand in for `/`.
pub trait HostProbe: Send + Sync {
    fn root(&self) -> &Path;
    fn run(
        &self,
        program: &'static str,
        args: &[&str],
        timeout: Duration,
        limit: usize,
    ) -> CommandOutcome;
    /// `device` is an absolute host path such as `/dev/kfd`.
    fn device_access(&self, device: &str) -> DeviceAccess;
    fn online_threads(&self) -> Option<u64> {
        None
    }
    fn usable_threads(&self) -> Option<u64> {
        None
    }
    fn os(&self) -> &'static str {
        host_os()
    }
    fn architecture(&self) -> &'static str {
        std::env::consts::ARCH
    }
    fn detection_supported(&self) -> bool {
        cfg!(target_os = "linux")
    }
}

pub fn host_os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "windows" => "windows",
        "macos" => "macos",
        other => other,
    }
}

fn cpu_family(architecture: &str) -> &str {
    match architecture {
        "aarch64" => "arm64",
        other => other,
    }
}

pub struct SystemProbe {
    root: PathBuf,
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }
}

impl HostProbe for SystemProbe {
    fn root(&self) -> &Path {
        &self.root
    }

    fn run(
        &self,
        program: &'static str,
        args: &[&str],
        timeout: Duration,
        limit: usize,
    ) -> CommandOutcome {
        run_bounded(program, args, timeout, limit)
    }

    fn device_access(&self, device: &str) -> DeviceAccess {
        posix_access(&self.root.join(device.trim_start_matches('/')))
    }

    fn online_threads(&self) -> Option<u64> {
        #[cfg(unix)]
        {
            let count = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
            u64::try_from(count).ok().filter(|count| *count > 0)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn usable_threads(&self) -> Option<u64> {
        std::thread::available_parallelism()
            .ok()
            .map(|count| count.get() as u64)
    }
}

/// Permission bits only: a device cgroup can still refuse open() after access() succeeds.
fn posix_access(path: &Path) -> DeviceAccess {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if std::fs::metadata(path).is_err() {
            return DeviceAccess::Missing;
        }
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return DeviceAccess::Unknown;
        };
        if unsafe { libc::access(name.as_ptr(), libc::R_OK | libc::W_OK) } == 0 {
            DeviceAccess::ReadWrite
        } else {
            DeviceAccess::Denied
        }
    }
    #[cfg(not(unix))]
    {
        match std::fs::metadata(path) {
            Ok(_) => DeviceAccess::Unknown,
            Err(_) => DeviceAccess::Missing,
        }
    }
}

/// Reads `commands/<program>.stdout`, `.status` or `.timeout` under the fixture root, and
/// treats a `<device>.denied` marker as an access refusal.
#[cfg(any(test, feature = "test-fixture"))]
pub struct FixtureProbe {
    pub root: PathBuf,
    pub os: &'static str,
    pub architecture: &'static str,
    pub supported: bool,
}

#[cfg(any(test, feature = "test-fixture"))]
impl FixtureProbe {
    pub fn linux(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            os: "linux",
            architecture: "x86_64",
            supported: true,
        }
    }
}

#[cfg(any(test, feature = "test-fixture"))]
impl HostProbe for FixtureProbe {
    fn root(&self) -> &Path {
        &self.root
    }

    fn run(
        &self,
        program: &'static str,
        _args: &[&str],
        _timeout: Duration,
        limit: usize,
    ) -> CommandOutcome {
        use std::io::Read;
        let base = self.root.join("commands");
        if base.join(format!("{program}.timeout")).exists() {
            return CommandOutcome::TimedOut;
        }
        if let Ok(status) = std::fs::read_to_string(base.join(format!("{program}.status"))) {
            return CommandOutcome::Failed(status.trim().parse().ok());
        }
        let Ok(file) = std::fs::File::open(base.join(format!("{program}.stdout"))) else {
            return CommandOutcome::NotFound;
        };
        let mut bytes = Vec::new();
        if file.take(limit as u64 + 1).read_to_end(&mut bytes).is_err() {
            return CommandOutcome::SpawnFailed;
        }
        if bytes.len() > limit {
            return CommandOutcome::OutputLimit;
        }
        String::from_utf8(bytes).map_or(CommandOutcome::InvalidOutput, CommandOutcome::Output)
    }

    fn device_access(&self, device: &str) -> DeviceAccess {
        let path = self.root.join(device.trim_start_matches('/'));
        let mut marker = path.clone().into_os_string();
        marker.push(".denied");
        if Path::new(&marker).exists() {
            DeviceAccess::Denied
        } else if path.exists() {
            DeviceAccess::ReadWrite
        } else {
            DeviceAccess::Missing
        }
    }

    fn os(&self) -> &'static str {
        self.os
    }

    fn architecture(&self) -> &'static str {
        self.architecture
    }

    fn detection_supported(&self) -> bool {
        self.supported
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Detection {
    Supported,
    Incomplete,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostIdentity {
    pub os: &'static str,
    pub architecture: &'static str,
    pub cpu_family: &'static str,
    pub detection: Detection,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Cpu {
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub physical_cores: Option<u64>,
    pub logical_threads: Option<u64>,
    pub online_threads: Option<u64>,
    pub usable_threads: Option<u64>,
    pub isa: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Memory {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
}

#[derive(Clone, Debug, Serialize)]
pub struct Gpu {
    pub vendor: GpuVendor,
    pub index: Option<u32>,
    pub name: Option<String>,
    pub uuid: Option<String>,
    pub pci_slot: Option<String>,
    pub pci_device_id: Option<String>,
    pub driver: Option<String>,
    pub driver_version: Option<String>,
    pub integrated: Option<bool>,
    pub integrated_basis: Option<&'static str>,
    pub vram_total_bytes: Option<u64>,
    pub vram_free_bytes: Option<u64>,
    pub gtt_total_bytes: Option<u64>,
    pub compute_capability: Option<String>,
    pub gfx_target: Option<String>,
    pub family: Option<String>,
    pub render_node: Option<String>,
    pub render_node_access: Option<DeviceAccess>,
    pub sources: Vec<&'static str>,
}

impl Gpu {
    fn new(vendor: GpuVendor) -> Self {
        Self {
            vendor,
            index: None,
            name: None,
            uuid: None,
            pci_slot: None,
            pci_device_id: None,
            driver: None,
            driver_version: None,
            integrated: None,
            integrated_basis: None,
            vram_total_bytes: None,
            vram_free_bytes: None,
            gtt_total_bytes: None,
            compute_capability: None,
            gfx_target: None,
            family: None,
            render_node: None,
            render_node_access: None,
            sources: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NpuVendor {
    Amd,
    Intel,
    Other,
}

#[derive(Clone, Debug, Serialize)]
pub struct Npu {
    pub vendor: NpuVendor,
    pub name: String,
    pub driver: Option<String>,
    pub family: Option<&'static str>,
    pub pci_device_id: Option<String>,
    pub device_node: String,
    pub access: DeviceAccess,
}

#[derive(Clone, Debug, Serialize)]
pub struct VulkanDevice {
    pub name: Option<String>,
    pub device_type: Option<String>,
    pub vendor_id: Option<String>,
    pub device_id: Option<String>,
    pub driver_name: Option<String>,
    pub api_version: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Vulkan {
    pub state: Availability,
    pub basis: &'static str,
    pub loader_found: Option<bool>,
    pub icd_manifests: Vec<String>,
    pub devices: Vec<VulkanDevice>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Rocm {
    pub kfd: DeviceAccess,
    pub kfd_access_basis: &'static str,
    pub topology_gpu_nodes: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Cuda {
    pub nvidia_smi: &'static str,
    pub control_device: DeviceAccess,
}

#[derive(Clone, Debug, Serialize)]
pub struct Devices {
    pub cpu: Option<Cpu>,
    pub memory: Memory,
    pub gpus: Vec<Gpu>,
    pub npus: Vec<Npu>,
    pub vulkan: Vulkan,
    pub rocm: Rocm,
    pub cuda: Cuda,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbeRecord {
    pub tool: &'static str,
    pub outcome: &'static str,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub source: String,
    pub code: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostReport {
    pub host: HostIdentity,
    pub devices: Devices,
    pub probes: Vec<ProbeRecord>,
    pub issues: Vec<Issue>,
}

impl HostReport {
    fn empty(probe: &dyn HostProbe, detection: Detection) -> Self {
        let architecture = probe.architecture();
        Self {
            host: HostIdentity {
                os: probe.os(),
                architecture,
                cpu_family: cpu_family(architecture),
                detection,
            },
            devices: Devices {
                cpu: None,
                memory: Memory {
                    total_bytes: None,
                    available_bytes: None,
                    source: "unavailable",
                },
                gpus: Vec::new(),
                npus: Vec::new(),
                vulkan: Vulkan {
                    state: Availability::Unknown,
                    basis: "not_probed",
                    loader_found: None,
                    icd_manifests: Vec::new(),
                    devices: Vec::new(),
                },
                rocm: Rocm {
                    kfd: DeviceAccess::Unknown,
                    kfd_access_basis: "not_probed",
                    topology_gpu_nodes: None,
                },
                cuda: Cuda {
                    nvidia_smi: "not_probed",
                    control_device: DeviceAccess::Unknown,
                },
            },
            probes: Vec::new(),
            issues: Vec::new(),
        }
    }
}

/// Blocking. Run on a blocking worker; the deadline is checked between reads and bounds
/// each helper command, but cannot interrupt a single kernel read.
pub fn detect(probe: &dyn HostProbe, deadline: Instant) -> HostReport {
    if !probe.detection_supported() {
        return HostReport::empty(probe, Detection::Unsupported);
    }
    let mut report = HostReport::empty(probe, Detection::Supported);
    sysfs::detect(probe, deadline, &mut report);
    report
}

pub struct Snapshot {
    pub report: HostReport,
    pub observed_at_unix_ms: u64,
    observed_at: Instant,
}

impl Snapshot {
    pub fn age(&self) -> Duration {
        self.observed_at.elapsed()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectionError {
    Deadline,
    Interrupted,
}

pub struct Detector {
    probe: Arc<dyn HostProbe>,
    ttl: Duration,
    budget: Duration,
    current: tokio::sync::Mutex<Option<Arc<Snapshot>>>,
}

impl Detector {
    pub fn new(probe: Arc<dyn HostProbe>, ttl: Duration, budget: Duration) -> Self {
        Self {
            probe,
            ttl,
            budget,
            current: tokio::sync::Mutex::new(None),
        }
    }

    pub fn from_environment() -> Self {
        #[cfg(feature = "test-fixture")]
        if let Some(root) = std::env::var_os("JUICED_TEST_HARDWARE_ROOT") {
            return Self::new(
                Arc::new(FixtureProbe::linux(root)),
                CACHE_TTL,
                DETECTION_BUDGET,
            );
        }
        Self::new(
            Arc::new(SystemProbe::default()),
            CACHE_TTL,
            DETECTION_BUDGET,
        )
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub async fn snapshot(&self) -> Result<Arc<Snapshot>, DetectionError> {
        let mut current = self.current.lock().await;
        if let Some(snapshot) = current.as_ref()
            && snapshot.age() < self.ttl
        {
            return Ok(snapshot.clone());
        }
        let probe = self.probe.clone();
        let budget = self.budget;
        let task = tokio::task::spawn_blocking(move || {
            let observed_at = Instant::now();
            let report = detect(probe.as_ref(), observed_at + budget);
            Snapshot {
                report,
                observed_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
                observed_at,
            }
        });
        let snapshot = match tokio::time::timeout(budget + RESULT_GRACE, task).await {
            Ok(Ok(snapshot)) => Arc::new(snapshot),
            Ok(Err(_)) => return Err(DetectionError::Interrupted),
            Err(_) => return Err(DetectionError::Deadline),
        };
        *current = Some(snapshot.clone());
        Ok(snapshot)
    }
}

#[derive(Serialize)]
pub struct SystemInfo<'a> {
    pub schema_version: u32,
    pub scope: &'static str,
    pub observed_at_unix_ms: u64,
    pub age_ms: u64,
    pub cache_ttl_ms: u64,
    #[serde(flatten)]
    pub report: &'a HostReport,
    #[serde(flatten)]
    pub inventory: Inventory,
}

impl<'a> SystemInfo<'a> {
    pub fn new(snapshot: &'a Snapshot, ttl: Duration, catalog: &crate::catalog::Catalog) -> Self {
        Self {
            schema_version: 1,
            scope: "Host hardware observations and native backend hardware suitability only. Installation, bundle identity, model fit and execution qualification are not established.",
            observed_at_unix_ms: snapshot.observed_at_unix_ms,
            age_ms: snapshot.age().as_millis().min(u64::MAX as u128) as u64,
            cache_ttl_ms: ttl.as_millis().min(u64::MAX as u128) as u64,
            report: &snapshot.report,
            inventory: project(catalog, &snapshot.report),
        }
    }
}
