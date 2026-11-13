use super::{Availability, Detection, DeviceAccess, GpuVendor, HostReport, NpuVendor};
use crate::catalog::{Catalog, NativeSupportRow};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    OsUnsupported,
    ArchitectureUnsupported,
    HostDetectionUnsupported,
    HostDetectionIncomplete,
    NoNvidiaGpu,
    NvidiaDriverUnverified,
    NvidiaComputeUnknown,
    NvidiaComputeUnsupported,
    NoAmdGpu,
    AmdGpuTargetUnknown,
    AmdGpuTargetUnsupported,
    KfdMissing,
    KfdInaccessible,
    KfdUnknown,
    RenderNodeInaccessible,
    NoAmdNpu,
    NpuFamilyUnknown,
    NpuFamilyUnsupported,
    NpuInaccessible,
    MemlockLimited,
    VulkanUnavailable,
    VulkanUnverified,
    InstallationProvenanceUnresolved,
    CpuInstructionsMissing,
    DeviceDeclarationUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub code: ReasonCode,
    pub message: String,
}

fn reason(code: ReasonCode, message: impl Into<String>) -> Reason {
    Reason {
        code,
        message: message.into(),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendVariant {
    pub recipe: String,
    pub display_name: String,
    pub backend: String,
    pub implementation_id: String,
    pub tier: String,
    pub format: String,
    pub os: Vec<String>,
    pub device_declaration: String,
    pub state: Availability,
    pub reasons: Vec<Reason>,
    pub devices: Vec<&'static str>,
    pub installed: Option<bool>,
    pub recommended: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    pub backends: Vec<BackendVariant>,
    pub recommendations: BTreeMap<String, Option<String>>,
}

enum Outcome {
    Match(&'static str),
    Miss(Reason),
    Unknown(Reason),
}

fn family_matches(pattern: &str, actual: &str) -> bool {
    match pattern.strip_suffix('X') {
        Some(prefix) => {
            actual.len() == pattern.len()
                && actual
                    .get(..prefix.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        }
        None => pattern.eq_ignore_ascii_case(actual),
    }
}

fn accepts(families: &[&str], actual: Option<&str>) -> Option<bool> {
    if families.is_empty() {
        return Some(true);
    }
    actual.map(|actual| families.iter().any(|family| family_matches(family, actual)))
}

fn parse_declaration(declaration: &str) -> Option<Vec<(&str, Vec<&str>)>> {
    declaration
        .split(';')
        .map(|part| {
            let part = part.trim();
            match part.split_once('(') {
                Some((class, rest)) => {
                    let families = rest.strip_suffix(')')?;
                    Some((
                        class.trim(),
                        families
                            .split(',')
                            .map(str::trim)
                            .filter(|family| !family.is_empty())
                            .collect(),
                    ))
                }
                None if !part.is_empty() => Some((part, Vec::new())),
                None => None,
            }
        })
        .collect()
}

fn list(families: &[&str]) -> String {
    families.join(", ")
}

fn requirement(class: &str, families: &[&str], host: &HostReport) -> Outcome {
    let devices = &host.devices;
    match class {
        "cpu" => match accepts(families, Some(host.host.cpu_family)) {
            Some(true) => Outcome::Match("cpu"),
            _ => Outcome::Miss(reason(
                ReasonCode::ArchitectureUnsupported,
                format!(
                    "Requires a {} CPU; this host is {}.",
                    list(families),
                    host.host.cpu_family
                ),
            )),
        },
        "nvidia_gpu" => {
            let gpus: Vec<_> = devices
                .gpus
                .iter()
                .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
                .collect();
            if gpus.is_empty() {
                return if matches!(
                    devices.cuda.nvidia_smi,
                    "timed_out" | "failed" | "output_limit" | "invalid_output" | "spawn_failed"
                ) {
                    Outcome::Unknown(reason(
                        ReasonCode::NvidiaDriverUnverified,
                        format!(
                            "nvidia-smi {}; NVIDIA GPU presence is unverified.",
                            devices.cuda.nvidia_smi
                        ),
                    ))
                } else {
                    Outcome::Miss(reason(ReasonCode::NoNvidiaGpu, "No NVIDIA GPU detected."))
                };
            }
            let verdicts: Vec<_> = gpus
                .iter()
                .map(|gpu| accepts(families, gpu.family.as_deref()))
                .collect();
            if verdicts.contains(&Some(true)) {
                Outcome::Match("nvidia_gpu")
            } else if verdicts.contains(&None) {
                Outcome::Unknown(reason(
                    ReasonCode::NvidiaComputeUnknown,
                    "NVIDIA GPU compute capability is unknown.",
                ))
            } else {
                Outcome::Miss(reason(
                    ReasonCode::NvidiaComputeUnsupported,
                    format!(
                        "Detected {}; requires one of {}.",
                        gpus.iter()
                            .filter_map(|gpu| gpu.family.as_deref())
                            .collect::<Vec<_>>()
                            .join(", "),
                        list(families)
                    ),
                ))
            }
        }
        "amd_gpu" => {
            let gpus: Vec<_> = devices
                .gpus
                .iter()
                .filter(|gpu| gpu.vendor == GpuVendor::Amd)
                .collect();
            if gpus.is_empty() {
                return Outcome::Miss(reason(ReasonCode::NoAmdGpu, "No AMD GPU detected."));
            }
            let verdicts: Vec<_> = gpus
                .iter()
                .map(|gpu| accepts(families, gpu.family.as_deref()))
                .collect();
            if verdicts.contains(&Some(true)) {
                Outcome::Match("amd_gpu")
            } else if verdicts.contains(&None) {
                Outcome::Unknown(reason(
                    ReasonCode::AmdGpuTargetUnknown,
                    "AMD GPU gfx target is unknown without KFD topology.",
                ))
            } else {
                Outcome::Miss(reason(
                    ReasonCode::AmdGpuTargetUnsupported,
                    format!(
                        "Detected {}; requires one of {}.",
                        gpus.iter()
                            .filter_map(|gpu| gpu.family.as_deref())
                            .collect::<Vec<_>>()
                            .join(", "),
                        list(families)
                    ),
                ))
            }
        }
        "amd_npu" => {
            let npus: Vec<_> = devices
                .npus
                .iter()
                .filter(|npu| npu.vendor == NpuVendor::Amd)
                .collect();
            if npus.is_empty() {
                return Outcome::Miss(reason(ReasonCode::NoAmdNpu, "No AMD NPU detected."));
            }
            let verdicts: Vec<_> = npus
                .iter()
                .map(|npu| accepts(families, npu.family))
                .collect();
            if verdicts.contains(&Some(true)) {
                Outcome::Match("amd_npu")
            } else if verdicts.contains(&None) {
                Outcome::Unknown(reason(
                    ReasonCode::NpuFamilyUnknown,
                    "AMD NPU generation is unknown.",
                ))
            } else {
                Outcome::Miss(reason(
                    ReasonCode::NpuFamilyUnsupported,
                    format!("Requires an AMD {} NPU.", list(families)),
                ))
            }
        }
        "metal" => Outcome::Miss(reason(ReasonCode::OsUnsupported, "Requires macOS Metal.")),
        _ => Outcome::Unknown(reason(
            ReasonCode::DeviceDeclarationUnknown,
            format!("Unrecognized device declaration {class}."),
        )),
    }
}

struct Evaluation {
    state: Availability,
    reasons: Vec<(Availability, Reason)>,
    devices: Vec<&'static str>,
}

impl Evaluation {
    fn single(state: Availability, reason: Reason) -> Self {
        Self {
            state,
            reasons: vec![(state, reason)],
            devices: Vec::new(),
        }
    }

    fn limit(&mut self, state: Availability, reason: Reason) {
        self.state = self.state.min(state);
        if !self
            .reasons
            .iter()
            .any(|(level, existing)| *level == state && existing.code == reason.code)
        {
            self.reasons.push((state, reason));
        }
    }

    /// Only reasons at the final level explain the result; weaker ones are superseded.
    fn finish(self) -> (Availability, Vec<Reason>, Vec<&'static str>) {
        let state = self.state;
        let reasons = self
            .reasons
            .into_iter()
            .filter(|(level, _)| *level == state)
            .map(|(_, reason)| reason)
            .collect();
        let devices = if state == Availability::Unavailable {
            Vec::new()
        } else {
            self.devices
        };
        (state, reasons, devices)
    }
}

fn devices(declaration: &str, all_required: bool, host: &HostReport) -> Evaluation {
    let Some(requirements) = parse_declaration(declaration) else {
        return Evaluation::single(
            Availability::Unknown,
            reason(
                ReasonCode::DeviceDeclarationUnknown,
                "The native device declaration could not be interpreted.",
            ),
        );
    };
    let mut matched = Vec::new();
    let mut misses = Vec::new();
    let mut unknowns = Vec::new();
    for (class, families) in &requirements {
        match requirement(class, families, host) {
            Outcome::Match(device) => {
                if !matched.contains(&device) {
                    matched.push(device)
                }
            }
            Outcome::Miss(reason) => misses.push(reason),
            Outcome::Unknown(reason) => unknowns.push(reason),
        }
    }
    let state = if all_required {
        if !misses.is_empty() {
            Availability::Unavailable
        } else if !unknowns.is_empty() {
            Availability::Unknown
        } else {
            Availability::Available
        }
    } else if !matched.is_empty() {
        Availability::Available
    } else if !unknowns.is_empty() {
        Availability::Unknown
    } else {
        Availability::Unavailable
    };
    if state == Availability::Available {
        misses.clear();
        unknowns.clear();
    }
    let reasons = misses
        .into_iter()
        .map(|reason| (Availability::Unavailable, reason))
        .chain(
            unknowns
                .into_iter()
                .map(|reason| (Availability::Unknown, reason)),
        )
        .collect();
    Evaluation {
        state,
        reasons,
        devices: matched,
    }
}

fn gate(backend: &str, host: &HostReport, evaluation: &mut Evaluation) {
    let devices = &host.devices;
    let amd = backend == "rocm" || backend == "hrx" || backend.starts_with("hybrid");
    if backend == "cuda" && devices.cuda.nvidia_smi != "ok" {
        evaluation.limit(
            Availability::Unknown,
            reason(
                ReasonCode::NvidiaDriverUnverified,
                format!(
                    "nvidia-smi {}; the NVIDIA driver is unverified.",
                    devices.cuda.nvidia_smi.replace('_', " ")
                ),
            ),
        );
    }
    if amd {
        match devices.rocm.kfd {
            DeviceAccess::ReadWrite => {}
            DeviceAccess::Missing => evaluation.limit(
                Availability::Unavailable,
                reason(ReasonCode::KfdMissing, "/dev/kfd is not present."),
            ),
            DeviceAccess::Denied => evaluation.limit(
                Availability::Unavailable,
                reason(
                    ReasonCode::KfdInaccessible,
                    "/dev/kfd is not accessible to the service.",
                ),
            ),
            DeviceAccess::Unknown => evaluation.limit(
                Availability::Unknown,
                reason(ReasonCode::KfdUnknown, "/dev/kfd access is unknown."),
            ),
        }
        let nodes: Vec<_> = devices
            .gpus
            .iter()
            .filter(|gpu| gpu.vendor == GpuVendor::Amd)
            .filter_map(|gpu| gpu.render_node_access)
            .collect();
        if !nodes.is_empty()
            && nodes
                .iter()
                .all(|access| *access != DeviceAccess::ReadWrite)
        {
            evaluation.limit(
                Availability::Unavailable,
                reason(
                    ReasonCode::RenderNodeInaccessible,
                    "No AMD GPU render node under /dev/dri is accessible to the service.",
                ),
            );
        }
    }
    if backend == "npu" || backend.starts_with("hybrid") {
        let npus: Vec<_> = devices
            .npus
            .iter()
            .filter(|npu| npu.vendor == NpuVendor::Amd)
            .collect();
        if !npus.is_empty() && npus.iter().all(|npu| npu.access != DeviceAccess::ReadWrite) {
            evaluation.limit(
                Availability::Unavailable,
                reason(
                    ReasonCode::NpuInaccessible,
                    "No AMD NPU device under /dev/accel is accessible to the service.",
                ),
            );
        }
        if !npus.is_empty() && devices.limits.memlock == "limited" {
            evaluation.limit(
                Availability::Unknown,
                reason(
                    ReasonCode::MemlockLimited,
                    format!(
                        "The locked-memory limit is {} bytes; AMD NPU engines pin buffers and normally need an unlimited limit (container: --ulimit memlock=-1:-1).",
                        devices.limits.memlock_bytes.unwrap_or_default()
                    ),
                ),
            );
        }
    }
    if backend == "vulkan" {
        match devices.vulkan.state {
            Availability::Available => {}
            Availability::Unknown => evaluation.limit(
                Availability::Unknown,
                reason(
                    ReasonCode::VulkanUnverified,
                    "Vulkan devices were not enumerated; Vulkan support is unverified.",
                ),
            ),
            Availability::Unavailable => evaluation.limit(
                Availability::Unavailable,
                reason(
                    ReasonCode::VulkanUnavailable,
                    "No usable Vulkan loader or device was found.",
                ),
            ),
        }
    }
    if backend == "system" {
        evaluation.limit(
            Availability::Unknown,
            reason(
                ReasonCode::InstallationProvenanceUnresolved,
                "system names installation provenance; the installed executable's build and compute path are not inventoried.",
            ),
        );
    }
}

fn os_label(os: &str) -> &str {
    match os {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

fn evaluate(backend: &str, row: &NativeSupportRow, host: &HostReport) -> Evaluation {
    if !row.os.iter().any(|os| os == host.host.os) {
        return Evaluation::single(
            Availability::Unavailable,
            reason(
                ReasonCode::OsUnsupported,
                format!(
                    "Requires {}.",
                    row.os
                        .iter()
                        .map(|os| os_label(os))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
            ),
        );
    }
    if host.host.detection == Detection::Unsupported {
        return Evaluation::single(
            Availability::Unknown,
            reason(
                ReasonCode::HostDetectionUnsupported,
                format!(
                    "Hardware detection is not implemented on {}.",
                    os_label(host.host.os)
                ),
            ),
        );
    }
    let mut evaluation = devices(&row.device_declaration, backend.starts_with("hybrid"), host);
    if evaluation.state != Availability::Unavailable {
        gate(backend, host, &mut evaluation);
    }
    if host.host.detection == Detection::Incomplete && evaluation.state != Availability::Available {
        evaluation.state = Availability::Unknown;
        evaluation.limit(
            Availability::Unknown,
            reason(
                ReasonCode::HostDetectionIncomplete,
                "Hardware detection reached its deadline before every probe completed.",
            ),
        );
    }
    evaluation
}

/// Native Juiced ranks Vulkan above ROCm for every engine. llama.cpp keeps ROCm first because
/// its chat models run there on AMD; image generation and transcription keep native's order.
pub fn preference(variant: &BackendVariant) -> Option<u8> {
    if variant.state != Availability::Available
        || variant.tier != "core"
        || variant.backend == "system"
        || variant.backend.starts_with("hybrid")
    {
        return None;
    }
    let vulkan_first = matches!(variant.recipe.as_str(), "sd-cpp" | "whispercpp");
    match variant.backend.as_str() {
        "cuda" => Some(0),
        "rocm" if vulkan_first => Some(4),
        "rocm" => Some(1),
        "metal" => Some(2),
        "npu" => Some(3),
        "vulkan"
            if variant
                .devices
                .iter()
                .any(|device| *device == "amd_gpu" || *device == "nvidia_gpu") =>
        {
            Some(if vulkan_first { 1 } else { 4 })
        }
        "cpu" => Some(5),
        _ => None,
    }
}

pub fn project(catalog: &Catalog, host: &HostReport) -> Inventory {
    let mut backends = Vec::new();
    let mut recommendations = BTreeMap::new();
    for migration in catalog.native_recipe_migrations() {
        let start = backends.len();
        let supplemental: Vec<NativeSupportRow> =
            crate::engines::catalog::SUPPLEMENTAL_SUPPORT_ROWS
                .iter()
                .filter(|row| row.recipe == migration.legacy_recipe)
                .map(|row| NativeSupportRow {
                    backend: row.backend.into(),
                    tier: row.tier.into(),
                    format: row.format.into(),
                    os: row.os.iter().map(|os| (*os).into()).collect(),
                    device_declaration: row.device_declaration.into(),
                })
                .collect();
        let mut labels = migration.native_backend_labels.clone();
        labels.extend(
            supplemental
                .iter()
                .map(|row| row.backend.clone())
                .filter(|backend| !migration.native_backend_labels.contains(backend)),
        );
        for label in &labels {
            let rows: Vec<_> = migration
                .native_support_rows
                .iter()
                .chain(&supplemental)
                .filter(|row| &row.backend == label)
                .collect();
            let implementation_id = migration
                .profile_overrides
                .iter()
                .find(|profile| &profile.legacy_backend == label)
                .map_or(&migration.catalog_entry, |profile| &profile.catalog_entry)
                .clone();
            let selected = rows
                .iter()
                .find(|row| row.os.iter().any(|os| os == host.host.os))
                .or(rows.first());
            let Some(row) = selected else {
                backends.push(BackendVariant {
                    recipe: migration.legacy_recipe.clone(),
                    display_name: migration.display_name.clone(),
                    backend: label.clone(),
                    implementation_id,
                    tier: "unknown".into(),
                    format: "unknown".into(),
                    os: Vec::new(),
                    device_declaration: String::new(),
                    state: Availability::Unknown,
                    reasons: vec![reason(
                        ReasonCode::DeviceDeclarationUnknown,
                        "No native support row declares this backend.",
                    )],
                    devices: Vec::new(),
                    installed: None,
                    recommended: false,
                });
                continue;
            };
            let mut os = Vec::new();
            for candidate in &rows {
                for value in &candidate.os {
                    if !os.contains(value) {
                        os.push(value.clone());
                    }
                }
            }
            let (state, reasons, devices) = evaluate(label, row, host).finish();
            backends.push(BackendVariant {
                recipe: migration.legacy_recipe.clone(),
                display_name: migration.display_name.clone(),
                backend: label.clone(),
                implementation_id,
                tier: row.tier.clone(),
                format: row.format.clone(),
                os,
                device_declaration: row.device_declaration.clone(),
                state,
                reasons,
                devices,
                installed: None,
                recommended: false,
            });
        }
        let best = backends[start..]
            .iter()
            .enumerate()
            .filter_map(|(index, variant)| preference(variant).map(|rank| (rank, index)))
            .min();
        let recommendation = best.map(|(_, index)| {
            let variant = &mut backends[start + index];
            variant.recommended = true;
            variant.backend.clone()
        });
        recommendations.insert(migration.legacy_recipe.clone(), recommendation);
    }
    Inventory {
        backends,
        recommendations,
    }
}
