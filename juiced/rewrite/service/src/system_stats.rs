use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, sync::watch};

const SMALL_FILE_BYTES: u64 = 4096;
const MEMINFO_BYTES: u64 = 16 * 1024;
const NVIDIA_OUTPUT_BYTES: u64 = 64 * 1024;
const MAXIMUM_DEVICES: usize = 16;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const NVIDIA_QUERY: &str = "--query-gpu=index,uuid,name,utilization.gpu,memory.used,memory.total";

#[derive(Clone, Debug)]
pub struct Config {
    pub proc_root: Option<PathBuf>,
    pub drm_root: Option<PathBuf>,
    pub nvidia_smi: Option<PathBuf>,
    pub interval: Duration,
    pub baseline_interval: Duration,
    pub idle_stop: Duration,
    pub nvidia_timeout: Duration,
    pub first_sample_wait: Duration,
}

impl Default for Config {
    fn default() -> Self {
        let linux = cfg!(target_os = "linux");
        Self {
            proc_root: linux.then(|| PathBuf::from("/proc")),
            drm_root: linux.then(|| PathBuf::from("/sys/class/drm")),
            nvidia_smi: linux.then(|| PathBuf::from("nvidia-smi")),
            interval: Duration::from_secs(2),
            baseline_interval: Duration::from_millis(250),
            idle_stop: Duration::from_secs(60),
            nvidia_timeout: Duration::from_secs(1),
            first_sample_wait: Duration::from_millis(1500),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceState {
    Observed,
    NotPresent,
    Unavailable,
    Error,
    Timeout,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gpu {
    pub id: String,
    pub vendor: &'static str,
    pub name: Option<String>,
    pub index: Option<u32>,
    pub utilization_percent: Option<f64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub shared_memory_used_bytes: Option<u64>,
    pub shared_memory_total_bytes: Option<u64>,
    pub utilization_source: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub sampled_at_ms: u64,
    pub observed: Instant,
    pub cpu_percent: Option<f64>,
    pub memory: Option<Memory>,
    pub gpus: Vec<Gpu>,
    pub nvidia: SourceState,
    pub amdgpu: SourceState,
}

pub fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
    let mut fields = stat.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values = fields
        .take(8)
        .map(|field| field.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if values.len() < 4 {
        return None;
    }
    let total = values
        .iter()
        .try_fold(0u64, |sum, value| sum.checked_add(*value))?;
    let idle = values[3].checked_add(values.get(4).copied().unwrap_or(0))?;
    Some(CpuTimes { total, idle })
}

pub fn cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<f64> {
    let total = current.total.checked_sub(previous.total)?;
    let idle = current.idle.checked_sub(previous.idle)?;
    if total == 0 || idle > total {
        return None;
    }
    Some(((total - idle) as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

pub fn parse_meminfo(text: &str) -> Option<Memory> {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let target = match fields.next() {
            Some("MemTotal:") => &mut total,
            Some("MemAvailable:") => &mut available,
            _ => continue,
        };
        if target.is_some() || fields.clone().nth(1) != Some("kB") {
            return None;
        }
        *target = Some(fields.next()?.parse::<u64>().ok()?.checked_mul(1024)?);
    }
    let (total, available) = (total?, available?);
    if total == 0 || available > total {
        return None;
    }
    Some(Memory {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total - available,
    })
}

fn optional_field(field: &str) -> Option<&str> {
    let field = field.trim();
    let unknown = field.is_empty()
        || field.starts_with('[')
        || field.eq_ignore_ascii_case("n/a")
        || field.eq_ignore_ascii_case("not supported");
    (!unknown).then_some(field)
}

fn percent(field: &str) -> Option<f64> {
    optional_field(field)?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
}

pub fn parse_nvidia(text: &str) -> Option<Vec<Gpu>> {
    let mut gpus = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if gpus.len() == MAXIMUM_DEVICES {
            break;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() < 6 {
            return None;
        }
        let tail = fields.len() - 3;
        let index = optional_field(fields[0]).and_then(|value| value.parse::<u32>().ok());
        let mib = |field: &str| {
            optional_field(field)?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024 * 1024)
        };
        let id = optional_field(fields[1])
            .map(str::to_owned)
            .or_else(|| index.map(|index| index.to_string()))?;
        gpus.push(Gpu {
            id: format!("nvidia:{id}"),
            vendor: "nvidia",
            name: optional_field(&fields[2..tail].join(",")).map(str::to_owned),
            index,
            utilization_percent: percent(fields[tail]),
            memory_used_bytes: mib(fields[tail + 1]),
            memory_total_bytes: mib(fields[tail + 2]),
            shared_memory_used_bytes: None,
            shared_memory_total_bytes: None,
            utilization_source: Some("nvidia-smi"),
        });
    }
    Some(gpus)
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(limit)
        .read_to_string(&mut text)?;
    Ok(text)
}

fn read_trimmed(path: &Path) -> Option<String> {
    read_bounded(path, SMALL_FILE_BYTES)
        .ok()
        .map(|text| text.trim().to_owned())
}

pub fn read_amdgpu(root: &Path) -> (Vec<Gpu>, SourceState) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), SourceState::Unavailable);
        }
        Err(_) => return (Vec::new(), SourceState::Error),
    };
    let mut cards: Vec<(u32, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let number = name.strip_prefix("card")?.parse::<u32>().ok()?;
            Some((number, name))
        })
        .collect();
    cards.sort();
    cards.truncate(MAXIMUM_DEVICES);
    let mut gpus = Vec::new();
    for (number, name) in cards {
        let device = root.join(&name).join("device");
        if read_trimmed(&device.join("vendor")).as_deref() != Some("0x1002") {
            continue;
        }
        let slot = read_bounded(&device.join("uevent"), SMALL_FILE_BYTES)
            .ok()
            .and_then(|uevent| {
                uevent
                    .lines()
                    .find_map(|line| line.strip_prefix("PCI_SLOT_NAME=").map(str::to_owned))
            });
        let suspended =
            read_trimmed(&device.join("power/runtime_status")).as_deref() == Some("suspended");
        let number_file = |file: &str| {
            if suspended {
                None
            } else {
                read_trimmed(&device.join(file))?.parse::<u64>().ok()
            }
        };
        let utilization = number_file("gpu_busy_percent")
            .filter(|value| *value <= 100)
            .map(|value| value as f64);
        gpus.push(Gpu {
            id: format!("amdgpu:{}", slot.unwrap_or(name)),
            vendor: "amd",
            name: None,
            index: Some(number),
            utilization_percent: utilization,
            memory_used_bytes: number_file("mem_info_vram_used"),
            memory_total_bytes: number_file("mem_info_vram_total"),
            shared_memory_used_bytes: number_file("mem_info_gtt_used"),
            shared_memory_total_bytes: number_file("mem_info_gtt_total"),
            utilization_source: Some(if suspended {
                "runtime-suspended"
            } else {
                "amdgpu-sysfs"
            }),
        });
    }
    let state = if gpus.is_empty() {
        SourceState::NotPresent
    } else {
        SourceState::Observed
    };
    (gpus, state)
}

pub async fn query_nvidia(program: &Path, timeout: Duration) -> Result<Vec<Gpu>, SourceState> {
    let mut command = tokio::process::Command::new(program);
    command
        .args([NVIDIA_QUERY, "--format=csv,noheader,nounits"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SourceState::Unavailable
        } else {
            SourceState::Error
        }
    })?;
    let mut stdout = child.stdout.take().ok_or(SourceState::Error)?;
    let result = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(NVIDIA_OUTPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > NVIDIA_OUTPUT_BYTES {
            return Ok((bytes, None));
        }
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((bytes, Some(status)))
    })
    .await;
    let (bytes, status) = match result {
        Err(_) => {
            let _ = child.start_kill();
            return Err(SourceState::Timeout);
        }
        Ok(Err(_)) => {
            let _ = child.start_kill();
            return Err(SourceState::Error);
        }
        Ok(Ok((_, None))) => {
            let _ = child.start_kill();
            return Err(SourceState::Error);
        }
        Ok(Ok((bytes, Some(status)))) => (bytes, status),
    };
    if !status.success() {
        return Err(SourceState::Error);
    }
    let text = String::from_utf8(bytes).map_err(|_| SourceState::Error)?;
    parse_nvidia(&text).ok_or(SourceState::Error)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(u64::MAX as u128) as u64
        })
}

fn read_cpu(proc_root: Option<&Path>) -> Option<CpuTimes> {
    parse_cpu_times(&read_bounded(&proc_root?.join("stat"), SMALL_FILE_BYTES).ok()?)
}

struct Collector {
    config: Config,
    cpu: Option<CpuTimes>,
    nvidia_missing: bool,
}

impl Collector {
    async fn baseline(&mut self) {
        let proc_root = self.config.proc_root.clone();
        self.cpu = tokio::task::spawn_blocking(move || read_cpu(proc_root.as_deref()))
            .await
            .ok()
            .flatten();
    }

    async fn collect(&mut self) -> Sample {
        let proc_root = self.config.proc_root.clone();
        let drm_root = self.config.drm_root.clone();
        let (cpu, memory, (amd, amdgpu)) = tokio::task::spawn_blocking(move || {
            let cpu = read_cpu(proc_root.as_deref());
            let memory = proc_root.as_deref().and_then(|root| {
                parse_meminfo(&read_bounded(&root.join("meminfo"), MEMINFO_BYTES).ok()?)
            });
            let amd = drm_root
                .as_deref()
                .map_or((Vec::new(), SourceState::Unsupported), read_amdgpu);
            (cpu, memory, amd)
        })
        .await
        .unwrap_or((None, None, (Vec::new(), SourceState::Error)));
        let cpu_percent = match (self.cpu, cpu) {
            (Some(previous), Some(current)) => cpu_percent(previous, current),
            _ => None,
        };
        self.cpu = cpu;
        let (mut gpus, nvidia) = match &self.config.nvidia_smi {
            None => (Vec::new(), SourceState::Unsupported),
            Some(_) if self.nvidia_missing => (Vec::new(), SourceState::Unavailable),
            Some(program) => match query_nvidia(program, self.config.nvidia_timeout).await {
                Ok(gpus) if gpus.is_empty() => (gpus, SourceState::NotPresent),
                Ok(gpus) => (gpus, SourceState::Observed),
                Err(state) => {
                    self.nvidia_missing = state == SourceState::Unavailable;
                    (Vec::new(), state)
                }
            },
        };
        gpus.extend(amd);
        Sample {
            sampled_at_ms: now_ms(),
            observed: Instant::now(),
            cpu_percent,
            memory,
            gpus,
            nvidia,
            amdgpu,
        }
    }
}

struct Shared {
    config: Config,
    latest: watch::Sender<Option<Arc<Sample>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    last_read: Mutex<Instant>,
    closed: AtomicBool,
    stop: tokio::sync::Notify,
}

impl Shared {
    fn idle(&self) -> bool {
        self.last_read
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .elapsed()
            > self.config.idle_stop
    }
}

pub struct Sampler {
    shared: Arc<Shared>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Sampler {
    pub fn new(config: Config) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                latest: watch::Sender::new(None),
                task: Mutex::new(None),
                last_read: Mutex::new(Instant::now()),
                closed: AtomicBool::new(false),
                stop: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn interval(&self) -> Duration {
        self.shared.config.interval
    }

    pub fn running(&self) -> bool {
        self.shared
            .task
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }

    fn ensure_running(&self) {
        if self.shared.closed.load(Ordering::Acquire) {
            return;
        }
        *self
            .shared
            .last_read
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Instant::now();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut task = self
            .shared
            .task
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if task.as_ref().is_some_and(|task| !task.is_finished())
            || self.shared.closed.load(Ordering::Acquire)
        {
            return;
        }
        *task = Some(runtime.spawn(run(self.shared.clone())));
    }

    pub async fn current(&self) -> Option<Arc<Sample>> {
        if self.shared.closed.load(Ordering::Acquire) {
            return None;
        }
        self.ensure_running();
        let mut latest = self.shared.latest.subscribe();
        tokio::time::timeout(
            self.shared.config.first_sample_wait,
            latest.wait_for(Option::is_some),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|sample| sample.clone())
    }

    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.stop.notify_one();
        if let Some(task) = self
            .shared
            .task
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            task.abort();
        }
        self.shared.latest.send_replace(None);
    }
}

async fn pause(shared: &Shared, duration: Duration) -> bool {
    tokio::select! {
        _ = shared.stop.notified() => false,
        _ = tokio::time::sleep(duration) => !shared.closed.load(Ordering::Acquire),
    }
}

async fn run(shared: Arc<Shared>) {
    let mut collector = Collector {
        config: shared.config.clone(),
        cpu: None,
        nvidia_missing: false,
    };
    collector.baseline().await;
    if !pause(&shared, shared.config.baseline_interval).await {
        return;
    }
    loop {
        let sample = tokio::select! {
            _ = shared.stop.notified() => return,
            sample = collector.collect() => sample,
        };
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        shared.latest.send_replace(Some(Arc::new(sample)));
        if !pause(&shared, shared.config.interval).await {
            return;
        }
        if shared.idle() {
            let mut task = shared
                .task
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if shared.idle() {
                *task = None;
                shared.latest.send_replace(None);
                return;
            }
        }
    }
}

fn gib(bytes: Option<u64>) -> Option<f64> {
    bytes.map(|bytes| bytes as f64 / GIB)
}

pub fn projection(sample: Option<&Sample>, interval: Duration) -> Value {
    let Some(sample) = sample else {
        return json!({
            "schema_version": 1, "supported": cfg!(target_os = "linux"),
            "sampled_at_ms": null, "sample_age_ms": null,
            "sample_interval_ms": interval.as_millis() as u64,
            "cpu_percent": null, "memory_gb": null, "memory_total_gb": null, "memory": null,
            "gpu_percent": null, "vram_gb": null, "vram_total_gb": null,
            "primary_gpu_id": null, "gpus": [], "npu_percent": null, "npu_source": null,
            "sources": {"cpu": "unavailable", "memory": "unavailable", "nvidia": "unavailable", "amdgpu": "unavailable", "npu": "unavailable"},
        });
    };
    let primary = sample.gpus.first();
    let gpus: Vec<Value> = sample
        .gpus
        .iter()
        .map(|gpu| {
            json!({
                "id": gpu.id, "vendor": gpu.vendor, "name": gpu.name, "index": gpu.index,
                "gpu_percent": gpu.utilization_percent,
                "memory_used_gb": gib(gpu.memory_used_bytes),
                "memory_total_gb": gib(gpu.memory_total_bytes),
                "memory_used_bytes": gpu.memory_used_bytes,
                "memory_total_bytes": gpu.memory_total_bytes,
                "shared_memory_used_bytes": gpu.shared_memory_used_bytes,
                "shared_memory_total_bytes": gpu.shared_memory_total_bytes,
                "integrated": null,
                "utilization_source": gpu.utilization_source,
            })
        })
        .collect();
    let observed = |present: bool| {
        if !cfg!(target_os = "linux") {
            SourceState::Unsupported
        } else if present {
            SourceState::Observed
        } else {
            SourceState::Unavailable
        }
    };
    json!({
        "schema_version": 1, "supported": cfg!(target_os = "linux"),
        "sampled_at_ms": sample.sampled_at_ms,
        "sample_age_ms": sample.observed.elapsed().as_millis().min(u64::MAX as u128) as u64,
        "sample_interval_ms": interval.as_millis() as u64,
        "cpu_percent": sample.cpu_percent,
        "memory_gb": gib(sample.memory.map(|memory| memory.used_bytes)),
        "memory_total_gb": gib(sample.memory.map(|memory| memory.total_bytes)),
        "memory": sample.memory.map(|memory| json!({
            "used_bytes": memory.used_bytes, "total_bytes": memory.total_bytes,
            "available_bytes": memory.available_bytes, "source": "proc-meminfo"
        })),
        "gpu_percent": primary.and_then(|gpu| gpu.utilization_percent),
        "vram_gb": gib(primary.and_then(|gpu| gpu.memory_used_bytes)),
        "vram_total_gb": gib(primary.and_then(|gpu| gpu.memory_total_bytes)),
        "primary_gpu_id": primary.map(|gpu| gpu.id.as_str()),
        "gpus": gpus,
        "npu_percent": null, "npu_source": null,
        "sources": {
            "cpu": observed(sample.cpu_percent.is_some()),
            "memory": observed(sample.memory.is_some()),
            "nvidia": sample.nvidia, "amdgpu": sample.amdgpu,
            "npu": if cfg!(target_os = "linux") { SourceState::Unavailable } else { SourceState::Unsupported },
        },
    })
}

pub fn name_gpus(stats: &mut Value, detected: &[crate::hardware::Gpu]) {
    let Some(gpus) = stats.get_mut("gpus").and_then(Value::as_array_mut) else {
        return;
    };
    for gpu in gpus {
        let Some(slot) = gpu["id"].as_str().and_then(|id| id.strip_prefix("amdgpu:")) else {
            continue;
        };
        let Some(device) = detected.iter().find(|device| {
            device.vendor == crate::hardware::GpuVendor::Amd
                && device.pci_slot.as_deref() == Some(slot)
        }) else {
            continue;
        };
        if gpu["name"].is_null()
            && let Some(name) = device.name.clone()
        {
            gpu["name"] = name.into();
        }
        if let Some(integrated) = device.integrated {
            gpu["integrated"] = integrated.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_delta_uses_idle_and_iowait_and_rejects_regressions() {
        let first =
            parse_cpu_times("cpu  100 0 100 700 100 0 0 0 50 0\ncpu0 1 2 3 4\nintr 1 2 3\n")
                .unwrap();
        assert_eq!(
            first,
            CpuTimes {
                total: 1000,
                idle: 800
            }
        );
        let second = parse_cpu_times("cpu  200 0 150 1500 150 0 0 0 99 0\n").unwrap();
        assert_eq!(cpu_percent(first, second), Some(15.0));
        assert_eq!(cpu_percent(first, first), None);
        assert_eq!(cpu_percent(second, first), None);
        assert_eq!(
            cpu_percent(
                first,
                CpuTimes {
                    total: 1100,
                    idle: 1000
                }
            ),
            None
        );
        assert_eq!(parse_cpu_times("cpu  1 2 3\n"), None);
        assert_eq!(parse_cpu_times("cpu0 1 2 3 4\n"), None);
        assert_eq!(parse_cpu_times("cpu  1 2 x 4\n"), None);
        assert_eq!(parse_cpu_times("cpu  18446744073709551615 1 1 1\n"), None);
        assert_eq!(
            parse_cpu_times("cpu  1 2 3 4"),
            Some(CpuTimes { total: 10, idle: 4 })
        );
    }

    #[test]
    fn meminfo_uses_available_memory_and_rejects_ambiguous_values() {
        let memory = parse_meminfo(
            "MemTotal:       32762664 kB\nMemFree:  1 kB\nMemAvailable:   26904584 kB\n",
        )
        .unwrap();
        assert_eq!(memory.total_bytes, 32762664 * 1024);
        assert_eq!(memory.available_bytes, 26904584 * 1024);
        assert_eq!(memory.used_bytes, (32762664 - 26904584) * 1024);
        for invalid in [
            "MemTotal: 100 kB\n",
            "MemTotal: 100 kB\nMemAvailable: 200 kB\n",
            "MemTotal: 100 kB\nMemTotal: 100 kB\nMemAvailable: 1 kB\n",
            "MemTotal: 100 MB\nMemAvailable: 1 kB\n",
            "MemTotal: 0 kB\nMemAvailable: 0 kB\n",
            "MemTotal: -1 kB\nMemAvailable: 0 kB\n",
            "MemTotal: 18446744073709551615 kB\nMemAvailable: 1 kB\n",
        ] {
            assert_eq!(parse_meminfo(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn nvidia_rows_keep_unknown_fields_null_and_reject_malformed_output() {
        let gpus = parse_nvidia(
            "0, GPU-2eef2439, NVIDIA GeForce RTX 5080, 37, 3447, 16303\n1, [N/A], Board, With Comma, [N/A], [Not Supported], 8192\n\n",
        )
        .unwrap();
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].id, "nvidia:GPU-2eef2439");
        assert_eq!(gpus[0].name.as_deref(), Some("NVIDIA GeForce RTX 5080"));
        assert_eq!(gpus[0].utilization_percent, Some(37.0));
        assert_eq!(gpus[0].memory_used_bytes, Some(3447 * 1024 * 1024));
        assert_eq!(gpus[0].memory_total_bytes, Some(16303 * 1024 * 1024));
        assert_eq!(gpus[1].id, "nvidia:1");
        assert_eq!(gpus[1].name.as_deref(), Some("Board,With Comma"));
        assert_eq!(gpus[1].utilization_percent, None);
        assert_eq!(gpus[1].memory_used_bytes, None);
        assert_eq!(gpus[1].memory_total_bytes, Some(8192 * 1024 * 1024));
        assert_eq!(parse_nvidia("").unwrap(), []);
        assert_eq!(parse_nvidia("0, GPU-1, name, 5"), None);
        assert_eq!(parse_nvidia("[N/A], [N/A], x, 1, 2, 3"), None);
        let bounded = "0, GPU-1, n, 1, 1, 1\n".repeat(MAXIMUM_DEVICES + 4);
        assert_eq!(parse_nvidia(&bounded).unwrap().len(), MAXIMUM_DEVICES);
        assert_eq!(
            parse_nvidia("0, GPU-1, n, 140, 1, 1").unwrap()[0].utilization_percent,
            None
        );
    }

    fn write(root: &Path, relative: &str, value: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }

    #[test]
    fn amdgpu_sysfs_reports_only_amd_cards_and_skips_suspended_reads() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write(root, "card0/device/vendor", "0x1002\n");
        write(
            root,
            "card0/device/uevent",
            "DRIVER=amdgpu\nPCI_SLOT_NAME=0000:03:00.0\n",
        );
        write(root, "card0/device/gpu_busy_percent", "42\n");
        write(root, "card0/device/mem_info_vram_used", "1073741824\n");
        write(root, "card0/device/mem_info_vram_total", "17163091968\n");
        write(root, "card0/device/mem_info_gtt_used", "37123694592\n");
        write(root, "card0/device/mem_info_gtt_total", "77309411328\n");
        write(root, "card0-DP-1/status", "connected\n");
        write(root, "card1/device/vendor", "0x10de\n");
        write(root, "card2/device/vendor", "0x1002\n");
        write(root, "card2/device/power/runtime_status", "suspended\n");
        write(root, "card2/device/gpu_busy_percent", "7\n");
        write(root, "card10/device/vendor", "0x1002\n");
        write(root, "card10/device/gpu_busy_percent", "garbage\n");
        write(root, "card10/device/mem_info_vram_total", "4096\n");
        let (gpus, state) = read_amdgpu(root);
        assert_eq!(state, SourceState::Observed);
        assert_eq!(gpus.len(), 3);
        assert_eq!(gpus[0].id, "amdgpu:0000:03:00.0");
        assert_eq!(gpus[0].utilization_percent, Some(42.0));
        assert_eq!(gpus[0].memory_used_bytes, Some(1073741824));
        assert_eq!(gpus[0].memory_total_bytes, Some(17163091968));
        assert_eq!(gpus[0].shared_memory_used_bytes, Some(37123694592));
        assert_eq!(gpus[0].shared_memory_total_bytes, Some(77309411328));
        assert_eq!(gpus[1].id, "amdgpu:card2");
        assert_eq!(gpus[1].utilization_percent, None);
        assert_eq!(gpus[1].utilization_source, Some("runtime-suspended"));
        assert_eq!(gpus[2].index, Some(10));
        assert_eq!(gpus[2].utilization_percent, None);
        assert_eq!(gpus[2].memory_used_bytes, None);
        assert_eq!(gpus[2].memory_total_bytes, Some(4096));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            read_amdgpu(empty.path()),
            (Vec::new(), SourceState::NotPresent)
        );
        assert_eq!(
            read_amdgpu(&empty.path().join("missing")),
            (Vec::new(), SourceState::Unavailable)
        );
    }

    #[cfg(unix)]
    fn script(directory: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nvidia_query_is_bounded_by_its_deadline_output_and_exit_status() {
        let directory = tempfile::tempdir().unwrap();
        let healthy = script(
            directory.path(),
            "healthy",
            "[ \"$1\" = \"--query-gpu=index,uuid,name,utilization.gpu,memory.used,memory.total\" ] || exit 3\necho '0, GPU-a, Fixture GPU, 12, 100, 200'",
        );
        let gpus = query_nvidia(&healthy, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(gpus[0].utilization_percent, Some(12.0));
        let slow = script(directory.path(), "slow", "exec sleep 30");
        let started = Instant::now();
        assert_eq!(
            query_nvidia(&slow, Duration::from_millis(200)).await,
            Err(SourceState::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let failing = script(
            directory.path(),
            "failing",
            "echo 'No devices were found'; exit 9",
        );
        assert_eq!(
            query_nvidia(&failing, Duration::from_secs(5)).await,
            Err(SourceState::Error)
        );
        let chatty = script(directory.path(), "chatty", "yes '0, GPU-a, n, 1, 1, 1'");
        assert_eq!(
            query_nvidia(&chatty, Duration::from_secs(5)).await,
            Err(SourceState::Error)
        );
        assert_eq!(
            query_nvidia(&directory.path().join("absent"), Duration::from_secs(1)).await,
            Err(SourceState::Unavailable)
        );
    }

    fn fixture_config(root: &Path) -> Config {
        write(root, "proc/stat", "cpu  100 0 100 700 100 0 0 0 0 0\n");
        write(
            root,
            "proc/meminfo",
            "MemTotal: 1000 kB\nMemAvailable: 250 kB\n",
        );
        write(root, "drm/card0/device/vendor", "0x1002\n");
        write(root, "drm/card0/device/gpu_busy_percent", "5\n");
        Config {
            proc_root: Some(root.join("proc")),
            drm_root: Some(root.join("drm")),
            nvidia_smi: Some(root.join("no-nvidia-smi")),
            interval: Duration::from_millis(20),
            baseline_interval: Duration::from_millis(10),
            idle_stop: Duration::from_millis(150),
            nvidia_timeout: Duration::from_millis(200),
            first_sample_wait: Duration::from_secs(2),
        }
    }

    #[tokio::test]
    async fn sampler_publishes_deltas_stops_when_idle_and_stays_closed() {
        let directory = tempfile::tempdir().unwrap();
        let sampler = Sampler::new(fixture_config(directory.path()));
        assert!(!sampler.running());
        let first = sampler.current().await.expect("first sample");
        assert!(sampler.running());
        assert_eq!(first.cpu_percent, None);
        assert_eq!(first.memory.unwrap().used_bytes, 750 * 1024);
        assert_eq!(first.nvidia, SourceState::Unavailable);
        assert_eq!(first.amdgpu, SourceState::Observed);
        write(
            directory.path(),
            "proc/stat",
            "cpu  200 0 150 1500 150 0 0 0 0 0\n",
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while sampler.current().await.unwrap().cpu_percent != Some(15.0) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("delta sample");
        let projected = projection(sampler.current().await.as_deref(), sampler.interval());
        assert_eq!(projected["cpu_percent"], 15.0);
        assert_eq!(projected["npu_percent"], Value::Null);
        assert_eq!(projected["primary_gpu_id"], "amdgpu:card0");
        assert_eq!(projected["gpus"][0]["memory_used_gb"], Value::Null);
        tokio::time::timeout(Duration::from_secs(3), async {
            while sampler.running() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("idle sampler stops");
        assert!(sampler.shared.latest.borrow().is_none());
        assert!(sampler.current().await.is_some());
        assert!(sampler.running());
        sampler.close();
        assert!(!sampler.running());
        assert!(sampler.current().await.is_none());
        assert!(!sampler.running());
    }

    #[test]
    fn projection_reports_null_rather_than_zero_without_a_sample() {
        let value = projection(None, Duration::from_secs(2));
        for field in [
            "cpu_percent",
            "memory_gb",
            "gpu_percent",
            "vram_gb",
            "npu_percent",
        ] {
            assert_eq!(value[field], Value::Null, "{field}");
        }
        assert_eq!(value["gpus"], json!([]));
    }

    #[test]
    fn amd_gpu_names_come_from_the_matching_detected_card() {
        use crate::hardware::{Gpu as Detected, GpuVendor};
        let mut radeon = Detected::new(GpuVendor::Amd);
        radeon.pci_slot = Some("0000:c4:00.0".into());
        radeon.name = Some("AMD Radeon 890M Graphics".into());
        radeon.integrated = Some(true);
        let mut other = Detected::new(GpuVendor::Amd);
        other.pci_slot = Some("0000:03:00.0".into());
        let mut stats = json!({"gpus": [
            {"id": "amdgpu:0000:c4:00.0", "name": null},
            {"id": "amdgpu:0000:03:00.0", "name": null},
            {"id": "nvidia:GPU-a", "name": "Kept"},
            {"id": "amdgpu:card9", "name": null},
        ]});
        name_gpus(&mut stats, &[other, radeon]);
        assert_eq!(stats["gpus"][0]["name"], "AMD Radeon 890M Graphics");
        assert_eq!(stats["gpus"][0]["integrated"], true);
        assert_eq!(stats["gpus"][1]["name"], Value::Null);
        assert_eq!(stats["gpus"][2]["name"], "Kept");
        assert_eq!(stats["gpus"][3]["name"], Value::Null);
    }
}
