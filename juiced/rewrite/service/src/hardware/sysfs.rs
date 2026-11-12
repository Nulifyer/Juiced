use super::{
    Availability, COMMAND_TIMEOUT, CommandOutcome, Cpu, Detection, Gpu, GpuVendor, HostProbe,
    HostReport, Issue, NVIDIA_SMI_OUTPUT_BYTES, Npu, NpuVendor, ProbeRecord,
    VULKANINFO_OUTPUT_BYTES, VulkanDevice,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    time::{Duration, Instant},
};

const FILE_BYTES: usize = 64 * 1024;
pub(super) const CPUINFO_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_CPUS: usize = 8192;
const MAX_GPUS: usize = 64;
const MAX_VULKAN_DEVICES: usize = 32;
const MAX_TEXT_FIELD: usize = 256;
const PCI_AMD_GPU: u16 = 0x1002;
const PCI_AMD_CPU: u16 = 0x1022;
const PCI_NVIDIA: u16 = 0x10de;
const PCI_INTEL: u16 = 0x8086;

pub(super) struct Reader<'a> {
    probe: &'a dyn HostProbe,
    deadline: Instant,
    issues: Vec<Issue>,
    expired: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReadFailure {
    Missing,
    Unreadable,
    Limit,
    Invalid,
    Deadline,
}

impl<'a> Reader<'a> {
    pub(super) fn new(probe: &'a dyn HostProbe, deadline: Instant) -> Self {
        Self {
            probe,
            deadline,
            issues: Vec::new(),
            expired: false,
        }
    }

    fn live(&mut self) -> bool {
        if !self.expired && Instant::now() >= self.deadline {
            self.expired = true;
            self.issues.push(Issue {
                source: "detection".into(),
                code: "deadline",
            });
        }
        !self.expired
    }

    fn issue(&mut self, source: &str, code: &'static str) {
        if self.issues.len() < 128 {
            self.issues.push(Issue {
                source: source.to_owned(),
                code,
            });
        }
    }

    fn read(&mut self, relative: &str, limit: usize) -> Result<String, ReadFailure> {
        if !self.live() {
            return Err(ReadFailure::Deadline);
        }
        let path = self.probe.root().join(relative);
        let file = std::fs::File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ReadFailure::Missing
            } else {
                ReadFailure::Unreadable
            }
        });
        let result = file.and_then(|file| {
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ReadFailure::Unreadable)?;
            if bytes.len() > limit {
                return Err(ReadFailure::Limit);
            }
            String::from_utf8(bytes).map_err(|_| ReadFailure::Invalid)
        });
        match &result {
            Err(ReadFailure::Limit) => self.issue(&format!("/{relative}"), "read_limit"),
            Err(ReadFailure::Invalid) => self.issue(&format!("/{relative}"), "invalid_encoding"),
            Err(ReadFailure::Unreadable) => self.issue(&format!("/{relative}"), "unreadable"),
            _ => {}
        }
        result
    }

    fn text(&mut self, relative: &str) -> Option<String> {
        self.read(relative, FILE_BYTES).ok()
    }

    fn exists(&self, relative: &str) -> bool {
        self.probe.root().join(relative).exists()
    }

    fn entries(&mut self, relative: &str, accept: impl Fn(&str) -> bool) -> Vec<String> {
        if !self.live() {
            return Vec::new();
        }
        let Ok(directory) = std::fs::read_dir(self.probe.root().join(relative)) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for entry in directory.take(MAX_ENTRIES * 8).flatten() {
            if let Some(name) = entry.file_name().to_str()
                && accept(name)
            {
                if names.len() == MAX_ENTRIES {
                    self.issue(&format!("/{relative}"), "entry_limit");
                    break;
                }
                names.push(name.to_owned());
            }
        }
        names.sort_by_key(|name| natural_key(name));
        names
    }

    fn link_name(&self, relative: &str) -> Option<String> {
        std::fs::read_link(self.probe.root().join(relative))
            .ok()?
            .file_name()?
            .to_str()
            .map(str::to_owned)
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

fn natural_key(name: &str) -> (String, u64) {
    let split = name
        .rfind(|character: char| !character.is_ascii_digit())
        .map_or(0, |index| index + 1);
    (
        name[..split].to_owned(),
        name[split..].parse().unwrap_or(u64::MAX),
    )
}

fn numbered(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.len() <= 6 && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub(super) fn clean_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_TEXT_FIELD
        || value.chars().any(char::is_control)
        || matches!(value, "[N/A]" | "N/A" | "[Not Supported]" | "Not Supported")
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn hex_u16(value: &str) -> Option<u16> {
    let value = value.trim();
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

fn decimal_u64(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value.len() > 20 {
        return None;
    }
    value.parse().ok()
}

fn uevent(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .take(64)
        .filter_map(|line| line.split_once('='))
        .collect()
}

pub(super) fn normalize_pci_slot(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    let (domain, rest) = value.split_once(':')?;
    let domain = u32::from_str_radix(domain, 16).ok()?;
    let (bus, device_function) = rest.split_once(':')?;
    let (device, function) = device_function.split_once('.')?;
    let valid = |part: &str, length: usize| {
        part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    (valid(bus, 2) && valid(device, 2) && valid(function, 1))
        .then(|| format!("{domain:04x}:{bus}:{device}.{function}"))
}

pub(super) fn detect(probe: &dyn HostProbe, deadline: Instant, report: &mut HostReport) {
    let mut reader = Reader::new(probe, deadline);
    report.devices.cpu = Some(cpu(&mut reader));
    memory(&mut reader, report);
    let nvidia = nvidia_smi(&mut reader, report);
    let vulkan = vulkan(&mut reader, report);
    gpus(&mut reader, report, nvidia, &vulkan);
    npus(&mut reader, report);
    report.devices.limits = match probe.memlock_limit() {
        super::MemlockLimit::Unlimited => super::ProcessLimits {
            memlock: "unlimited",
            memlock_bytes: None,
        },
        super::MemlockLimit::Bytes(bytes) => super::ProcessLimits {
            memlock: "limited",
            memlock_bytes: Some(bytes),
        },
        super::MemlockLimit::Unknown => super::ProcessLimits {
            memlock: "unknown",
            memlock_bytes: None,
        },
    };
    report.devices.cuda.control_device = probe.device_access("/dev/nvidiactl");
    report.issues.append(&mut reader.issues);
    if reader.expired {
        report.host.detection = Detection::Incomplete;
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CpuInfo {
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub logical: Option<u64>,
    pub physical: Option<u64>,
    pub isa: Vec<&'static str>,
}

const X86_FLAGS: &[(&str, &str)] = &[
    ("sse4_2", "sse4_2"),
    ("avx", "avx"),
    ("avx2", "avx2"),
    ("fma", "fma"),
    ("f16c", "f16c"),
    ("avx_vnni", "avx_vnni"),
    ("avx512f", "avx512f"),
    ("avx512bw", "avx512bw"),
    ("avx512vl", "avx512vl"),
    ("avx512_vnni", "avx512_vnni"),
    ("avx512_bf16", "avx512_bf16"),
    ("avx512_fp16", "avx512_fp16"),
    ("amx_tile", "amx"),
    ("amx_int8", "amx_int8"),
    ("amx_bf16", "amx_bf16"),
];

const ARM_FEATURES: &[(&str, &str)] = &[
    ("asimd", "neon"),
    ("asimdhp", "fp16"),
    ("asimddp", "dotprod"),
    ("i8mm", "i8mm"),
    ("bf16", "bf16"),
    ("sve", "sve"),
    ("sve2", "sve2"),
    ("sme", "sme"),
];

pub(super) fn parse_cpuinfo(text: &str) -> CpuInfo {
    let mut info = CpuInfo::default();
    let mut processors = 0u64;
    let mut cores = BTreeSet::new();
    let mut identified = 0u64;
    let mut package = None;
    let mut core = None;
    let mut flags: Option<(&[(&str, &str)], &str)> = None;
    let finish = |package: &mut Option<String>,
                  core: &mut Option<String>,
                  cores: &mut BTreeSet<(String, String)>,
                  identified: &mut u64| {
        if let (Some(package), Some(core)) = (package.take(), core.take()) {
            cores.insert((package, core));
            *identified += 1;
        }
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "processor" => {
                finish(&mut package, &mut core, &mut cores, &mut identified);
                if processors as usize >= MAX_CPUS {
                    break;
                }
                processors += 1;
            }
            "model name" if info.name.is_none() => info.name = clean_text(value),
            "vendor_id" if info.vendor.is_none() => {
                info.vendor = match value {
                    "AuthenticAMD" => Some("amd".into()),
                    "GenuineIntel" => Some("intel".into()),
                    other => clean_text(other),
                }
            }
            "physical id" => package = Some(value.to_owned()),
            "core id" => core = Some(value.to_owned()),
            "flags" if flags.is_none() => flags = Some((X86_FLAGS, value)),
            "Features" if flags.is_none() => flags = Some((ARM_FEATURES, value)),
            _ => {}
        }
    }
    finish(&mut package, &mut core, &mut cores, &mut identified);
    if processors > 0 {
        info.logical = Some(processors);
        if identified == processors {
            info.physical = Some(cores.len() as u64);
        }
    }
    if let Some((table, value)) = flags {
        let present: BTreeSet<&str> = value.split_ascii_whitespace().collect();
        info.isa = table
            .iter()
            .filter(|(flag, _)| present.contains(flag))
            .map(|(_, label)| *label)
            .collect();
    }
    info
}

fn cpu(reader: &mut Reader<'_>) -> Cpu {
    let info = match reader.read("proc/cpuinfo", CPUINFO_BYTES) {
        Ok(text) => parse_cpuinfo(&text),
        Err(ReadFailure::Missing) => {
            reader.issue("/proc/cpuinfo", "missing");
            CpuInfo::default()
        }
        Err(_) => CpuInfo::default(),
    };
    let physical = info.physical.or_else(|| topology_cores(reader));
    let online = reader.probe.online_threads();
    Cpu {
        name: info.name,
        vendor: info.vendor,
        physical_cores: physical,
        logical_threads: info.logical.or(online),
        online_threads: online,
        usable_threads: reader.probe.usable_threads(),
        isa: info.isa,
    }
}

fn topology_cores(reader: &mut Reader<'_>) -> Option<u64> {
    let cpus = reader.entries("sys/devices/system/cpu", |name| numbered(name, "cpu"));
    if cpus.is_empty() {
        return None;
    }
    let mut cores = BTreeSet::new();
    for cpu in cpus.iter().take(MAX_CPUS) {
        let base = format!("sys/devices/system/cpu/{cpu}/topology");
        let package = reader
            .text(&format!("{base}/physical_package_id"))
            .and_then(|value| decimal_u64(&value));
        let core = reader
            .text(&format!("{base}/core_id"))
            .and_then(|value| decimal_u64(&value));
        cores.insert((package?, core?));
    }
    Some(cores.len() as u64)
}

pub(super) fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let mut total = None;
    let mut available = None;
    for line in text.lines().take(512) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let target = match key {
            "MemTotal" => &mut total,
            "MemAvailable" => &mut available,
            _ => continue,
        };
        let mut parts = value.split_ascii_whitespace();
        *target = match (parts.next(), parts.next(), parts.next()) {
            (Some(number), Some("kB"), None) => {
                decimal_u64(number).and_then(|kib| kib.checked_mul(1024))
            }
            _ => None,
        };
    }
    (total, available)
}

fn memory(reader: &mut Reader<'_>, report: &mut HostReport) {
    let Ok(text) = reader.read("proc/meminfo", FILE_BYTES) else {
        return;
    };
    let (total, available) = parse_meminfo(&text);
    if total.is_none() {
        reader.issue("/proc/meminfo", "invalid_metadata");
    }
    report.devices.memory.total_bytes = total;
    report.devices.memory.available_bytes = available.filter(|_| total.is_some());
    report.devices.memory.source = "/proc/meminfo";
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct NvidiaRow {
    pub index: Option<u32>,
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub total_mib: Option<u64>,
    pub free_mib: Option<u64>,
    pub compute_capability: Option<String>,
    pub driver_version: Option<String>,
    pub pci_slot: Option<String>,
}

pub(super) const NVIDIA_QUERY: &str =
    "index,uuid,name,memory.total,memory.free,compute_cap,driver_version,pci.bus_id";

pub(super) fn parse_nvidia_smi(text: &str) -> (Vec<NvidiaRow>, usize) {
    let mut rows = Vec::new();
    let mut rejected = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() != 8 || rows.len() == MAX_GPUS {
            rejected += 1;
            continue;
        }
        let compute_capability = clean_text(fields[5]).filter(|value| sm_family(value).is_some());
        rows.push(NvidiaRow {
            index: fields[0].parse().ok(),
            uuid: clean_text(fields[1]),
            name: clean_text(fields[2]),
            total_mib: decimal_u64(fields[3]),
            free_mib: decimal_u64(fields[4]),
            compute_capability,
            driver_version: clean_text(fields[6]),
            pci_slot: normalize_pci_slot(fields[7]),
        });
    }
    (rows, rejected)
}

pub(super) fn sm_family(compute_capability: &str) -> Option<String> {
    let (major, minor) = compute_capability.split_once('.')?;
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    (major > 0 && major < 100 && minor < 10).then(|| format!("sm_{major}{minor}"))
}

fn probe_command(
    reader: &mut Reader<'_>,
    report: &mut HostReport,
    tool: &'static str,
    args: &[&str],
    limit: usize,
) -> CommandOutcome {
    if !reader.live() {
        return CommandOutcome::TimedOut;
    }
    let started = Instant::now();
    let outcome = reader
        .probe
        .run(tool, args, COMMAND_TIMEOUT.min(reader.remaining()), limit);
    report.probes.push(ProbeRecord {
        tool,
        outcome: outcome.label(),
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    });
    outcome
}

fn nvidia_smi(reader: &mut Reader<'_>, report: &mut HostReport) -> Vec<NvidiaRow> {
    let query = format!("--query-gpu={NVIDIA_QUERY}");
    let outcome = probe_command(
        reader,
        report,
        "nvidia-smi",
        &[&query, "--format=csv,noheader,nounits"],
        NVIDIA_SMI_OUTPUT_BYTES,
    );
    report.devices.cuda.nvidia_smi = outcome.label();
    let CommandOutcome::Output(text) = outcome else {
        return Vec::new();
    };
    let (rows, rejected) = parse_nvidia_smi(&text);
    if rejected > 0 {
        reader.issue("nvidia-smi", "invalid_output");
    }
    if rows.is_empty() {
        report.devices.cuda.nvidia_smi = "no_devices";
    }
    rows
}

pub(super) fn parse_vulkaninfo(text: &str) -> Vec<VulkanDevice> {
    let mut devices = Vec::new();
    let mut in_devices = false;
    let mut current: Option<VulkanDevice> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "Devices:" {
            in_devices = true;
            continue;
        }
        if !in_devices {
            continue;
        }
        let is_header = trimmed
            .strip_prefix("GPU")
            .and_then(|rest| rest.strip_suffix(':'))
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        if is_header {
            if let Some(device) = current.take() {
                devices.push(device);
            }
            if devices.len() == MAX_VULKAN_DEVICES {
                break;
            }
            current = Some(VulkanDevice {
                name: None,
                device_type: None,
                vendor_id: None,
                device_id: None,
                driver_name: None,
                api_version: None,
            });
            continue;
        }
        let (Some(device), Some((key, value))) = (current.as_mut(), trimmed.split_once('=')) else {
            continue;
        };
        let value = clean_text(value);
        match key.trim() {
            "deviceName" => device.name = value,
            "deviceType" => {
                device.device_type = value.map(|value| {
                    value
                        .strip_prefix("PHYSICAL_DEVICE_TYPE_")
                        .unwrap_or(&value)
                        .to_ascii_lowercase()
                })
            }
            "vendorID" => device.vendor_id = value.filter(|value| hex_u16(value).is_some()),
            "deviceID" => device.device_id = value.filter(|value| hex_u16(value).is_some()),
            "driverName" => device.driver_name = value,
            "apiVersion" => device.api_version = value,
            _ => {}
        }
    }
    if let Some(device) = current.take()
        && devices.len() < MAX_VULKAN_DEVICES
    {
        devices.push(device);
    }
    devices
}

const VULKAN_ICD_DIRECTORIES: &[&str] = &[
    "etc/vulkan/icd.d",
    "usr/share/vulkan/icd.d",
    "usr/local/share/vulkan/icd.d",
];

const VULKAN_LOADER_PATHS: &[&str] = &[
    "usr/lib/libvulkan.so.1",
    "usr/lib64/libvulkan.so.1",
    "usr/lib/x86_64-linux-gnu/libvulkan.so.1",
    "usr/lib/aarch64-linux-gnu/libvulkan.so.1",
    "lib/x86_64-linux-gnu/libvulkan.so.1",
    "lib/aarch64-linux-gnu/libvulkan.so.1",
    "usr/local/lib/libvulkan.so.1",
];

fn vulkan(reader: &mut Reader<'_>, report: &mut HostReport) -> Vec<VulkanDevice> {
    let mut manifests = BTreeSet::new();
    for directory in VULKAN_ICD_DIRECTORIES {
        for name in reader.entries(directory, |name| name.ends_with(".json")) {
            if manifests.len() < 64 {
                manifests.insert(name);
            }
        }
    }
    let loader = VULKAN_LOADER_PATHS.iter().any(|path| reader.exists(path));
    let outcome = probe_command(
        reader,
        report,
        "vulkaninfo",
        &["--summary"],
        VULKANINFO_OUTPUT_BYTES,
    );
    let vulkan = &mut report.devices.vulkan;
    vulkan.icd_manifests = manifests.into_iter().collect();
    vulkan.loader_found = Some(loader);
    let (state, basis, devices) = match outcome {
        CommandOutcome::Output(text) => {
            let devices = parse_vulkaninfo(&text);
            if devices.is_empty() {
                (Availability::Unavailable, "vulkaninfo_no_devices", devices)
            } else {
                (Availability::Available, "vulkaninfo", devices)
            }
        }
        CommandOutcome::TimedOut | CommandOutcome::OutputLimit | CommandOutcome::InvalidOutput => {
            (Availability::Unknown, "vulkaninfo_inconclusive", Vec::new())
        }
        _ if vulkan.icd_manifests.is_empty() && !loader => {
            (Availability::Unavailable, "no_loader_or_icd", Vec::new())
        }
        CommandOutcome::Failed(_) if vulkan.icd_manifests.is_empty() => {
            (Availability::Unavailable, "vulkaninfo_failed", Vec::new())
        }
        _ => (
            Availability::Unknown,
            "icd_manifests_unverified",
            Vec::new(),
        ),
    };
    vulkan.state = state;
    vulkan.basis = basis;
    vulkan.devices = devices.clone();
    devices
}

pub(super) fn gfx_name(target_version: u64) -> Option<String> {
    let major = target_version / 10000;
    let minor = (target_version / 100) % 100;
    let stepping = target_version % 100;
    ((1..100).contains(&major) && minor < 16 && stepping < 16)
        .then(|| format!("gfx{major}{minor:x}{stepping:x}"))
}

#[derive(Debug)]
struct KfdNode {
    gfx: Option<String>,
    render_minor: Option<u64>,
    device_id: Option<u64>,
}

fn kfd_nodes(reader: &mut Reader<'_>) -> Option<Vec<KfdNode>> {
    let base = "sys/class/kfd/kfd/topology/nodes";
    if !reader.exists(base) {
        return None;
    }
    let mut nodes = Vec::new();
    for node in reader.entries(base, |name| {
        !name.is_empty() && name.len() <= 4 && name.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        let Some(text) = reader.text(&format!("{base}/{node}/properties")) else {
            continue;
        };
        let properties: BTreeMap<&str, u64> = text
            .lines()
            .take(256)
            .filter_map(|line| {
                let (key, value) = line.split_once(' ')?;
                Some((key, decimal_u64(value)?))
            })
            .collect();
        let Some(&version) = properties.get("gfx_target_version") else {
            continue;
        };
        if version == 0 {
            continue;
        }
        let gfx = gfx_name(version);
        if gfx.is_none() {
            reader.issue(&format!("/{base}/{node}/properties"), "invalid_metadata");
        }
        nodes.push(KfdNode {
            gfx,
            render_minor: properties.get("drm_render_minor").copied(),
            device_id: properties.get("device_id").copied(),
        });
    }
    Some(nodes)
}

const AMD_INTEGRATED_DEVICES: &[(u16, Option<&str>)] = &[
    (0x150e, Some("AMD Radeon 880M / 890M Graphics")),
    (0x1586, Some("AMD Radeon 8050S / 8060S Graphics")),
    (0x1114, Some("AMD Radeon 840M / 860M Graphics")),
    (0x15bf, Some("AMD Radeon 780M Graphics")),
    (0x15c8, Some("AMD Radeon 740M Graphics")),
    (0x1900, None),
    (0x1681, None),
    (0x164e, None),
    (0x13c0, None),
    (0x1636, None),
    (0x1638, None),
    (0x164c, None),
    (0x15e7, None),
    (0x15d8, None),
    (0x15dd, None),
    (0x1435, None),
];

const AMD_INTEGRATED_TARGETS: &[&str] = &[
    "gfx902", "gfx909", "gfx90c", "gfx1033", "gfx1035", "gfx1036", "gfx1103", "gfx1150", "gfx1151",
    "gfx1152", "gfx1153",
];

struct DrmCard {
    vendor: u16,
    device: Option<u16>,
    slot: Option<String>,
    driver: Option<String>,
    base: String,
    render_node: Option<String>,
    render_minor: Option<u64>,
}

fn drm_cards(reader: &mut Reader<'_>) -> Vec<DrmCard> {
    let mut cards = Vec::new();
    let mut slots = BTreeSet::new();
    for card in reader.entries("sys/class/drm", |name| numbered(name, "card")) {
        if cards.len() == MAX_GPUS {
            break;
        }
        let base = format!("sys/class/drm/{card}/device");
        let Some(vendor) = reader
            .text(&format!("{base}/vendor"))
            .and_then(|value| hex_u16(&value))
        else {
            continue;
        };
        if ![PCI_AMD_GPU, PCI_NVIDIA, PCI_INTEL].contains(&vendor) {
            continue;
        }
        let device = reader
            .text(&format!("{base}/device"))
            .and_then(|value| hex_u16(&value));
        let event = reader.text(&format!("{base}/uevent")).unwrap_or_default();
        let fields = uevent(&event);
        let slot = fields
            .get("PCI_SLOT_NAME")
            .and_then(|slot| normalize_pci_slot(slot));
        if let Some(slot) = &slot
            && !slots.insert(slot.clone())
        {
            continue;
        }
        let driver = fields
            .get("DRIVER")
            .and_then(|value| clean_text(value))
            .or_else(|| reader.link_name(&format!("{base}/driver")));
        let render = reader
            .entries(&format!("{base}/drm"), |name| numbered(name, "renderD"))
            .into_iter()
            .next();
        cards.push(DrmCard {
            vendor,
            device,
            slot,
            driver,
            render_minor: render
                .as_deref()
                .and_then(|name| name.strip_prefix("renderD"))
                .and_then(decimal_u64),
            render_node: render.map(|name| format!("/dev/dri/{name}")),
            base,
        });
    }
    cards
}

fn vulkan_name(vulkan: &[VulkanDevice], vendor: u16, device: Option<u16>) -> Option<String> {
    let device = device?;
    vulkan
        .iter()
        .find(|candidate| {
            candidate.vendor_id.as_deref().and_then(hex_u16) == Some(vendor)
                && candidate.device_id.as_deref().and_then(hex_u16) == Some(device)
        })
        .and_then(|candidate| candidate.name.clone())
}

fn gpus(
    reader: &mut Reader<'_>,
    report: &mut HostReport,
    mut nvidia: Vec<NvidiaRow>,
    vulkan: &[VulkanDevice],
) {
    let kfd = kfd_nodes(reader);
    report.devices.rocm.topology_gpu_nodes = kfd.as_ref().map(|nodes| nodes.len() as u32);
    report.devices.rocm.kfd = reader.probe.device_access("/dev/kfd");
    report.devices.rocm.kfd_access_basis = "access(2) read/write permission";
    let kfd = kfd.unwrap_or_default();
    let amd_count = |cards: &[DrmCard]| {
        cards
            .iter()
            .filter(|card| card.vendor == PCI_AMD_GPU)
            .count()
    };
    let cards = drm_cards(reader);
    let single_amd = amd_count(&cards) == 1 && kfd.len() == 1;
    let mut gpus = Vec::new();
    for card in &cards {
        let vendor = match card.vendor {
            PCI_AMD_GPU => GpuVendor::Amd,
            PCI_NVIDIA => GpuVendor::Nvidia,
            _ => GpuVendor::Intel,
        };
        let mut gpu = Gpu::new(vendor);
        gpu.pci_slot = card.slot.clone();
        gpu.pci_device_id = card.device.map(|device| format!("0x{device:04x}"));
        gpu.driver = card.driver.clone();
        gpu.render_node = card.render_node.clone();
        gpu.render_node_access = card
            .render_node
            .as_deref()
            .map(|node| reader.probe.device_access(node));
        gpu.name = vulkan_name(vulkan, card.vendor, card.device);
        gpu.sources.push("drm");
        match vendor {
            GpuVendor::Amd => {
                let byte = |name: &str, reader: &mut Reader<'_>| {
                    reader
                        .text(&format!("{}/{name}", card.base))
                        .and_then(|value| decimal_u64(&value))
                };
                let total = byte("mem_info_vram_total", reader);
                let used = byte("mem_info_vram_used", reader);
                gpu.vram_total_bytes = total;
                gpu.vram_free_bytes = total
                    .zip(used)
                    .and_then(|(total, used)| total.checked_sub(used));
                gpu.gtt_total_bytes = byte("mem_info_gtt_total", reader);
                let node = kfd
                    .iter()
                    .find(|node| {
                        node.render_minor.is_some() && node.render_minor == card.render_minor
                    })
                    .or_else(|| single_amd.then(|| &kfd[0]));
                if let Some(node) = node {
                    gpu.gfx_target = node.gfx.clone();
                    gpu.family = node.gfx.clone();
                    gpu.sources.push("kfd");
                    if gpu.pci_device_id.is_none() {
                        gpu.pci_device_id = node
                            .device_id
                            .and_then(|id| u16::try_from(id).ok())
                            .map(|id| format!("0x{id:04x}"));
                    }
                }
                let known = card
                    .device
                    .and_then(|device| AMD_INTEGRATED_DEVICES.iter().find(|(id, _)| *id == device));
                if gpu.name.is_none() {
                    gpu.name = known.and_then(|(_, name)| name.map(str::to_owned));
                }
                (gpu.integrated, gpu.integrated_basis) = if known.is_some() {
                    (Some(true), Some("pci_device_id"))
                } else if gpu
                    .gfx_target
                    .as_deref()
                    .is_some_and(|gfx| AMD_INTEGRATED_TARGETS.contains(&gfx))
                {
                    (Some(true), Some("gfx_target"))
                } else if reader.exists(&format!("{}/board_info", card.base)) {
                    (Some(false), Some("board_info"))
                } else {
                    (None, None)
                };
            }
            GpuVendor::Nvidia => {
                gpu.integrated = Some(false);
                gpu.integrated_basis = Some("vendor");
                if let Some(position) = nvidia
                    .iter()
                    .position(|row| row.pci_slot.is_some() && row.pci_slot == card.slot)
                {
                    apply_nvidia(&mut gpu, nvidia.remove(position));
                }
            }
            GpuVendor::Intel => {
                if let Some(slot) = &card.slot {
                    gpu.integrated = Some(slot == "0000:00:02.0");
                    gpu.integrated_basis = Some("pci_slot");
                }
            }
        }
        gpus.push(gpu);
    }
    for row in nvidia {
        let mut gpu = Gpu::new(GpuVendor::Nvidia);
        gpu.integrated = Some(false);
        gpu.integrated_basis = Some("vendor");
        gpu.pci_slot = row.pci_slot.clone();
        apply_nvidia(&mut gpu, row);
        gpus.push(gpu);
    }
    report.devices.gpus = gpus;
}

fn apply_nvidia(gpu: &mut Gpu, row: NvidiaRow) {
    const MIB: u64 = 1024 * 1024;
    gpu.index = row.index;
    gpu.uuid = row.uuid;
    gpu.name = row.name.or(gpu.name.take());
    gpu.vram_total_bytes = row.total_mib.and_then(|value| value.checked_mul(MIB));
    gpu.vram_free_bytes = row.free_mib.and_then(|value| value.checked_mul(MIB));
    gpu.family = row.compute_capability.as_deref().and_then(sm_family);
    gpu.compute_capability = row.compute_capability;
    gpu.driver_version = row.driver_version;
    gpu.sources.push("nvidia-smi");
}

fn npus(reader: &mut Reader<'_>, report: &mut HostReport) {
    for accel in reader.entries("sys/class/accel", |name| numbered(name, "accel")) {
        if report.devices.npus.len() == 16 {
            break;
        }
        let base = format!("sys/class/accel/{accel}/device");
        let event = reader.text(&format!("{base}/uevent")).unwrap_or_default();
        let driver = uevent(&event)
            .get("DRIVER")
            .and_then(|value| clean_text(value))
            .or_else(|| reader.link_name(&format!("{base}/driver")));
        let vendor = reader
            .text(&format!("{base}/vendor"))
            .and_then(|value| hex_u16(&value));
        let device = reader
            .text(&format!("{base}/device"))
            .and_then(|value| hex_u16(&value));
        let (vendor, name, family) = match driver.as_deref() {
            Some("amdxdna") => {
                let family = match device {
                    Some(0x17f0) => Some("XDNA2"),
                    Some(0x1502) => Some("XDNA"),
                    _ => None,
                };
                let name = if family == Some("XDNA2") {
                    "AMD XDNA 2 NPU"
                } else {
                    "AMD XDNA NPU"
                };
                (NpuVendor::Amd, name.to_owned(), family)
            }
            Some("intel_vpu") => (NpuVendor::Intel, "Intel NPU".to_owned(), None),
            other => (
                match vendor {
                    Some(PCI_AMD_CPU | PCI_AMD_GPU) => NpuVendor::Amd,
                    Some(PCI_INTEL) => NpuVendor::Intel,
                    _ => NpuVendor::Other,
                },
                other.map_or_else(|| "Unknown accelerator".to_owned(), str::to_owned),
                None,
            ),
        };
        let node = format!("/dev/accel/{accel}");
        let (firmware_version, driver_version) = if vendor == NpuVendor::Amd {
            (
                reader
                    .text(&format!("{base}/fw_version"))
                    .and_then(|value| clean_text(&value)),
                driver
                    .as_deref()
                    .filter(|driver| *driver == "amdxdna")
                    .and_then(|_| reader.text("sys/module/amdxdna/version"))
                    .and_then(|value| clean_text(&value)),
            )
        } else {
            (None, None)
        };
        report.devices.npus.push(Npu {
            firmware_version,
            driver_version,
            vendor,
            name,
            driver,
            family,
            pci_device_id: device.map(|device| format!("0x{device:04x}")),
            access: reader.probe.device_access(&node),
            device_node: node,
        });
    }
}
