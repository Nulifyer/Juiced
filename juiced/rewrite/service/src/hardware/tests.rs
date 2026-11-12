use super::*;
use crate::catalog::Catalog;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Tree(tempfile::TempDir);

impl Tree {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn file(&self, relative: &str, contents: impl AsRef<[u8]>) -> &Self {
        let path = self.0.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    fn dir(&self, relative: &str) -> &Self {
        std::fs::create_dir_all(self.0.path().join(relative)).unwrap();
        self
    }

    fn deny(&self, device: &str) -> &Self {
        self.file(&format!("{}.denied", device.trim_start_matches('/')), "")
    }

    fn command(&self, program: &str, stdout: &str) -> &Self {
        self.file(&format!("commands/{program}.stdout"), stdout)
    }

    fn probe(&self) -> FixtureProbe {
        FixtureProbe::linux(self.0.path())
    }
}

fn cpuinfo(name: &str, vendor: &str, cores: u32, threads_per_core: u32, flags: &str) -> String {
    let mut text = String::new();
    for processor in 0..cores * threads_per_core {
        text.push_str(&format!(
            "processor\t: {processor}\nvendor_id\t: {vendor}\nmodel name\t: {name}\nphysical id\t: 0\ncore id\t\t: {}\ncpu cores\t: {cores}\nflags\t\t: {flags}\n\n",
            processor % cores
        ));
    }
    text
}

const MEMINFO: &str =
    "MemTotal:       32763640 kB\nMemFree:         1000000 kB\nMemAvailable:   26446104 kB\n";
const ZEN2_FLAGS: &str = "fpu vme sse4_1 sse4_2 avx f16c fma avx2 sha_ni";
const ZEN5_FLAGS: &str =
    "fpu sse4_2 avx f16c fma avx2 avx_vnni avx512f avx512bw avx512vl avx512_vnni avx512_bf16";

fn nvidia_card(tree: &Tree, card: &str, slot: &str, render: &str) {
    tree.file(&format!("sys/class/drm/{card}/device/vendor"), "0x10de\n")
        .file(&format!("sys/class/drm/{card}/device/device"), "0x2c02\n")
        .file(
            &format!("sys/class/drm/{card}/device/uevent"),
            format!("DRIVER=nvidia\nPCI_ID=10DE:2C02\nPCI_SLOT_NAME={slot}\n"),
        )
        .dir(&format!("sys/class/drm/{card}/device/drm/{render}"))
        .file(&format!("dev/dri/{render}"), "");
}

const NVIDIA_SMI: &str = "0, GPU-2eef2439-a5c6-49c9-7af1-0b278dee0958, NVIDIA GeForce RTX 5080, 16303, 12482, 12.0, 620.70.06, 00000000:09:00.0\n";

const VULKAN_NVIDIA: &str = "==========\nVULKANINFO\n==========\n\nDevices:\n========\nGPU0:\n\tapiVersion         = 1.4.357\n\tdriverVersion      = 620.70.6.0\n\tvendorID           = 0x10de\n\tdeviceID           = 0x2c02\n\tdeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\n\tdeviceName         = NVIDIA GeForce RTX 5080\n\tdriverName         = NVIDIA\n";

const VULKAN_RADV: &str = "Devices:\n========\nGPU0:\n\tapiVersion         = 1.4.305\n\tvendorID           = 0x1002\n\tdeviceID           = 0x150e\n\tdeviceType         = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU\n\tdeviceName         = AMD Radeon 890M Graphics (RADV GFX1150)\n\tdriverName         = radv\nGPU1:\n\tvendorID           = 0x10005\n\tdeviceType         = PHYSICAL_DEVICE_TYPE_CPU\n\tdeviceName         = llvmpipe (LLVM 19.1.7, 256 bits)\n\tdriverName         = llvmpipe\n";

fn rtx_host() -> Tree {
    let tree = Tree::new();
    tree.file(
        "proc/cpuinfo",
        cpuinfo(
            "AMD Ryzen 7 3700X 8-Core Processor",
            "AuthenticAMD",
            8,
            2,
            ZEN2_FLAGS,
        ),
    )
    .file("proc/meminfo", MEMINFO)
    .file("dev/nvidiactl", "")
    .file("usr/lib/libvulkan.so.1", "")
    .file("usr/share/vulkan/icd.d/nvidia_icd.json", "{}")
    .command("nvidia-smi", NVIDIA_SMI)
    .command("vulkaninfo", VULKAN_NVIDIA);
    nvidia_card(&tree, "card1", "0000:09:00.0", "renderD128");
    tree.dir("sys/class/drm/card1-DP-1");
    tree
}

fn strix_host(kfd: bool) -> Tree {
    let tree = Tree::new();
    tree.file(
        "proc/cpuinfo",
        cpuinfo("AMD Ryzen AI 9 HX 370 w/ Radeon 890M", "AuthenticAMD", 12, 2, ZEN5_FLAGS),
    )
    .file("proc/meminfo", MEMINFO)
    .file("sys/class/drm/card1/device/vendor", "0x1002\n")
    .file("sys/class/drm/card1/device/device", "0x150e\n")
    .file(
        "sys/class/drm/card1/device/uevent",
        "DRIVER=amdgpu\nPCI_ID=1002:150E\nPCI_SLOT_NAME=0000:c4:00.0\n",
    )
    .file("sys/class/drm/card1/device/mem_info_vram_total", "536870912\n")
    .file("sys/class/drm/card1/device/mem_info_vram_used", "134217728\n")
    .file("sys/class/drm/card1/device/mem_info_gtt_total", "16685121536\n")
    .dir("sys/class/drm/card1/device/drm/renderD128")
    .file("dev/dri/renderD128", "")
    .file(
        "sys/class/kfd/kfd/topology/nodes/0/properties",
        "cpu_cores_count 24\nsimd_count 0\ngfx_target_version 0\nvendor_id 0\ndevice_id 0\n",
    )
    .file(
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "cpu_cores_count 0\nsimd_count 32\ngfx_target_version 110500\nvendor_id 4098\ndevice_id 5390\ndrm_render_minor 128\n",
    )
    .file(
        "sys/class/accel/accel0/device/uevent",
        "DRIVER=amdxdna\nPCI_ID=1022:17F0\nPCI_SLOT_NAME=0000:c5:00.1\n",
    )
    .file("sys/class/accel/accel0/device/vendor", "0x1022\n")
    .file("sys/class/accel/accel0/device/device", "0x17f0\n")
    .file("dev/accel/accel0", "")
    .file("usr/lib/x86_64-linux-gnu/libvulkan.so.1", "")
    .file("usr/share/vulkan/icd.d/radeon_icd.x86_64.json", "{}")
    .command("vulkaninfo", VULKAN_RADV);
    if kfd {
        tree.file("dev/kfd", "");
    }
    tree
}

fn report(tree: &Tree) -> HostReport {
    detect(&tree.probe(), Instant::now() + Duration::from_secs(5))
}

fn inventory(report: &HostReport) -> Inventory {
    project(&Catalog::from_seed().unwrap(), report)
}

fn variant<'a>(inventory: &'a Inventory, recipe: &str, backend: &str) -> &'a BackendVariant {
    inventory
        .backends
        .iter()
        .find(|variant| variant.recipe == recipe && variant.backend == backend)
        .unwrap_or_else(|| panic!("missing {recipe}:{backend}"))
}

fn codes(variant: &BackendVariant) -> Vec<ReasonCode> {
    variant.reasons.iter().map(|reason| reason.code).collect()
}

fn recommended<'a>(inventory: &'a Inventory, recipe: &str) -> Option<&'a str> {
    inventory.recommendations[recipe].as_deref()
}

fn assert_recommendations_are_core(inventory: &Inventory) {
    for variant in &inventory.backends {
        if variant.recommended {
            assert_eq!(
                variant.tier, "core",
                "{}:{}",
                variant.recipe, variant.backend
            );
            assert_eq!(variant.state, Availability::Available);
            assert!(!variant.backend.starts_with("hybrid") && variant.backend != "system");
        }
        assert_eq!(variant.installed, None);
    }
    let marked = inventory
        .backends
        .iter()
        .filter(|variant| variant.recommended)
        .count();
    assert_eq!(
        marked,
        inventory
            .recommendations
            .values()
            .filter(|value| value.is_some())
            .count()
    );
}

#[test]
fn nvidia_host_reports_cuda_device_and_recommends_cuda() {
    let tree = rtx_host();
    let report = report(&tree);
    assert_eq!(report.host.detection, Detection::Supported);
    let cpu = report.devices.cpu.as_ref().unwrap();
    assert_eq!(
        cpu.name.as_deref(),
        Some("AMD Ryzen 7 3700X 8-Core Processor")
    );
    assert_eq!(cpu.vendor.as_deref(), Some("amd"));
    assert_eq!(
        (cpu.physical_cores, cpu.logical_threads),
        (Some(8), Some(16))
    );
    assert_eq!(cpu.isa, ["sse4_2", "avx", "avx2", "fma", "f16c"]);
    assert_eq!(report.devices.memory.total_bytes, Some(32763640 * 1024));
    assert_eq!(report.devices.memory.available_bytes, Some(26446104 * 1024));
    assert_eq!(report.devices.gpus.len(), 1);
    let gpu = &report.devices.gpus[0];
    assert_eq!(gpu.vendor, GpuVendor::Nvidia);
    assert_eq!(gpu.name.as_deref(), Some("NVIDIA GeForce RTX 5080"));
    assert_eq!(gpu.vram_total_bytes, Some(16303 * 1024 * 1024));
    assert_eq!(gpu.vram_free_bytes, Some(12482 * 1024 * 1024));
    assert_eq!(gpu.compute_capability.as_deref(), Some("12.0"));
    assert_eq!(gpu.family.as_deref(), Some("sm_120"));
    assert_eq!(gpu.driver_version.as_deref(), Some("620.70.06"));
    assert_eq!(gpu.pci_slot.as_deref(), Some("0000:09:00.0"));
    assert_eq!(gpu.render_node.as_deref(), Some("/dev/dri/renderD128"));
    assert_eq!(gpu.render_node_access, Some(DeviceAccess::ReadWrite));
    assert_eq!(gpu.sources, ["drm", "nvidia-smi"]);
    assert_eq!(report.devices.cuda.nvidia_smi, "ok");
    assert_eq!(report.devices.cuda.control_device, DeviceAccess::ReadWrite);
    assert_eq!(report.devices.rocm.kfd, DeviceAccess::Missing);
    assert_eq!(report.devices.vulkan.state, Availability::Available);
    assert_eq!(report.devices.vulkan.devices.len(), 1);
    assert_eq!(
        report.devices.vulkan.devices[0].device_type.as_deref(),
        Some("discrete_gpu")
    );
    assert!(report.devices.npus.is_empty());
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(
        report
            .probes
            .iter()
            .map(|probe| (probe.tool, probe.outcome))
            .collect::<Vec<_>>(),
        [("nvidia-smi", "ok"), ("vulkaninfo", "ok")]
    );

    let inventory = inventory(&report);
    assert_eq!(inventory.backends.len(), 40);
    assert_eq!(inventory.recommendations.len(), 18);
    let cuda = variant(&inventory, "llamacpp", "cuda");
    assert_eq!(cuda.state, Availability::Available);
    assert_eq!(cuda.devices, ["nvidia_gpu"]);
    assert_eq!(recommended(&inventory, "llamacpp"), Some("cuda"));
    assert_eq!(recommended(&inventory, "sd-cpp"), Some("cuda"));
    assert_eq!(recommended(&inventory, "whispercpp"), Some("cpu"));
    assert_eq!(recommended(&inventory, "acestep"), None);
    assert_eq!(
        variant(&inventory, "acestep", "cuda").state,
        Availability::Available
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "rocm")),
        [ReasonCode::NoAmdGpu]
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "metal")),
        [ReasonCode::OsUnsupported]
    );
    assert_eq!(
        variant(&inventory, "llamacpp", "metal").reasons[0].message,
        "Requires macOS."
    );
    assert_eq!(
        codes(variant(&inventory, "flm", "npu")),
        [ReasonCode::NoAmdNpu]
    );
    assert_eq!(
        codes(variant(&inventory, "ryzenai-llm", "npu")),
        [ReasonCode::OsUnsupported]
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "hybrid-rocm")),
        [ReasonCode::NoAmdGpu, ReasonCode::NoAmdNpu]
    );
    let system = variant(&inventory, "llamacpp", "system");
    assert_eq!(system.state, Availability::Unknown);
    assert_eq!(
        codes(system),
        [ReasonCode::InstallationProvenanceUnresolved]
    );
    let vulkan = variant(&inventory, "llamacpp", "vulkan");
    assert_eq!(
        (vulkan.state, vulkan.devices.as_slice()),
        (Availability::Available, ["cpu"].as_slice())
    );
    assert_eq!(
        variant(&inventory, "sd-cpp", "vulkan").devices,
        ["cpu", "nvidia_gpu"]
    );
    assert_eq!(
        variant(&inventory, "llamacpp", "hybrid-rocm").implementation_id,
        "llama-xdna-hybrid"
    );
    let moonshine = variant(&inventory, "moonshine", "cpu");
    assert_eq!(moonshine.os, ["windows", "linux", "macos"]);
    assert_eq!(moonshine.state, Availability::Available);
    assert_recommendations_are_core(&inventory);
}

#[test]
fn absent_nvidia_tools_and_vulkan_fall_back_to_cpu() {
    let tree = Tree::new();
    tree.file(
        "proc/cpuinfo",
        cpuinfo(
            "AMD Ryzen 7 3700X 8-Core Processor",
            "AuthenticAMD",
            8,
            2,
            ZEN2_FLAGS,
        ),
    )
    .file("proc/meminfo", MEMINFO);
    let report = report(&tree);
    assert!(report.devices.gpus.is_empty());
    assert_eq!(report.devices.cuda.nvidia_smi, "not_found");
    assert_eq!(report.devices.cuda.control_device, DeviceAccess::Missing);
    assert_eq!(report.devices.vulkan.state, Availability::Unavailable);
    assert_eq!(report.devices.vulkan.basis, "no_loader_or_icd");
    assert_eq!(report.devices.rocm.topology_gpu_nodes, None);
    let inventory = inventory(&report);
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "cuda")),
        [ReasonCode::NoNvidiaGpu]
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "vulkan")),
        [ReasonCode::VulkanUnavailable]
    );
    assert_eq!(recommended(&inventory, "llamacpp"), Some("cpu"));
    assert_recommendations_are_core(&inventory);
}

#[test]
fn strix_point_igpu_npu_and_kfd_select_rocm_without_recommending_hybrid() {
    let tree = strix_host(true);
    let report = report(&tree);
    let cpu = report.devices.cpu.as_ref().unwrap();
    assert_eq!(
        (cpu.physical_cores, cpu.logical_threads),
        (Some(12), Some(24))
    );
    assert!(cpu.isa.contains(&"avx512f") && cpu.isa.contains(&"avx512_bf16"));
    assert!(!cpu.isa.contains(&"amx"));
    assert_eq!(report.devices.gpus.len(), 1);
    let gpu = &report.devices.gpus[0];
    assert_eq!(gpu.vendor, GpuVendor::Amd);
    assert_eq!(gpu.gfx_target.as_deref(), Some("gfx1150"));
    assert_eq!(gpu.family.as_deref(), Some("gfx1150"));
    assert_eq!(gpu.integrated, Some(true));
    assert_eq!(gpu.integrated_basis, Some("pci_device_id"));
    assert_eq!(
        gpu.name.as_deref(),
        Some("AMD Radeon 890M Graphics (RADV GFX1150)")
    );
    assert_eq!(gpu.pci_device_id.as_deref(), Some("0x150e"));
    assert_eq!(gpu.driver.as_deref(), Some("amdgpu"));
    assert_eq!(gpu.vram_total_bytes, Some(536870912));
    assert_eq!(gpu.vram_free_bytes, Some(536870912 - 134217728));
    assert_eq!(gpu.gtt_total_bytes, Some(16685121536));
    assert_eq!(gpu.sources, ["drm", "kfd"]);
    assert_eq!(report.devices.rocm.kfd, DeviceAccess::ReadWrite);
    assert_eq!(report.devices.rocm.topology_gpu_nodes, Some(1));
    assert_eq!(report.devices.npus.len(), 1);
    let npu = &report.devices.npus[0];
    assert_eq!(npu.vendor, NpuVendor::Amd);
    assert_eq!(npu.driver.as_deref(), Some("amdxdna"));
    assert_eq!(npu.family, Some("XDNA2"));
    assert_eq!(npu.device_node, "/dev/accel/accel0");
    assert_eq!(npu.access, DeviceAccess::ReadWrite);
    assert_eq!(report.devices.vulkan.devices.len(), 2);
    assert_eq!(report.devices.cuda.nvidia_smi, "not_found");

    let inventory = inventory(&report);
    assert_eq!(
        variant(&inventory, "llamacpp", "rocm").state,
        Availability::Available
    );
    assert_eq!(recommended(&inventory, "llamacpp"), Some("rocm"));
    assert_eq!(recommended(&inventory, "whispercpp"), Some("rocm"));
    assert_eq!(recommended(&inventory, "sd-cpp"), Some("rocm"));
    assert_eq!(recommended(&inventory, "flm"), Some("npu"));
    assert_eq!(variant(&inventory, "flm", "npu").devices, ["amd_npu"]);
    let hybrid = variant(&inventory, "llamacpp", "hybrid-rocm");
    assert_eq!(hybrid.state, Availability::Available);
    assert_eq!(hybrid.devices, ["amd_gpu", "amd_npu"]);
    assert!(!hybrid.recommended);
    let vllm = variant(&inventory, "vllm", "rocm");
    assert_eq!(vllm.state, Availability::Available);
    assert_eq!(recommended(&inventory, "vllm"), None);
    assert_eq!(
        codes(variant(&inventory, "ds4", "rocm")),
        [ReasonCode::AmdGpuTargetUnsupported]
    );
    assert_eq!(
        variant(&inventory, "ds4", "rocm").reasons[0].message,
        "Detected gfx1150; requires one of gfx1151."
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp-hrx", "hrx")),
        [ReasonCode::AmdGpuTargetUnsupported]
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "cuda")),
        [ReasonCode::NoNvidiaGpu]
    );
    assert_eq!(
        variant(&inventory, "llamacpp", "vulkan").devices,
        ["amd_gpu", "cpu"]
    );
    assert_recommendations_are_core(&inventory);
}

#[test]
fn amd_without_kfd_access_keeps_target_but_refuses_rocm() {
    for (denied, code) in [
        (false, ReasonCode::KfdMissing),
        (true, ReasonCode::KfdInaccessible),
    ] {
        let tree = strix_host(denied);
        if denied {
            tree.deny("/dev/kfd");
        }
        let report = report(&tree);
        assert_eq!(
            report.devices.rocm.kfd,
            if denied {
                DeviceAccess::Denied
            } else {
                DeviceAccess::Missing
            }
        );
        assert_eq!(
            report.devices.gpus[0].gfx_target.as_deref(),
            Some("gfx1150")
        );
        let inventory = inventory(&report);
        for (recipe, backend) in [
            ("llamacpp", "rocm"),
            ("whispercpp", "rocm"),
            ("llamacpp", "hybrid-rocm"),
        ] {
            let variant = variant(&inventory, recipe, backend);
            assert_eq!(
                variant.state,
                Availability::Unavailable,
                "{recipe}:{backend}"
            );
            assert_eq!(codes(variant), [code]);
        }
        assert_eq!(recommended(&inventory, "llamacpp"), Some("vulkan"));
        assert_eq!(recommended(&inventory, "flm"), Some("npu"));
        assert_recommendations_are_core(&inventory);
    }
}

#[test]
fn inaccessible_render_node_and_npu_device_are_reported() {
    let tree = strix_host(true);
    tree.deny("/dev/dri/renderD128").deny("/dev/accel/accel0");
    let report = report(&tree);
    assert_eq!(
        report.devices.gpus[0].render_node_access,
        Some(DeviceAccess::Denied)
    );
    assert_eq!(report.devices.npus[0].access, DeviceAccess::Denied);
    let inventory = inventory(&report);
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "rocm")),
        [ReasonCode::RenderNodeInaccessible]
    );
    assert_eq!(
        codes(variant(&inventory, "flm", "npu")),
        [ReasonCode::NpuInaccessible]
    );
    assert_eq!(recommended(&inventory, "flm"), None);
}

#[test]
fn amd_without_kfd_topology_leaves_target_unknown() {
    let tree = strix_host(false);
    std::fs::remove_dir_all(tree.0.path().join("sys/class/kfd")).unwrap();
    std::fs::remove_file(tree.0.path().join("sys/class/drm/card1/device/device")).unwrap();
    std::fs::remove_dir_all(tree.0.path().join("commands")).unwrap();
    let report = report(&tree);
    let gpu = &report.devices.gpus[0];
    assert_eq!(gpu.gfx_target, None);
    assert_eq!(gpu.integrated, None);
    assert_eq!(gpu.name, None);
    let inventory = inventory(&report);
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "rocm")),
        [ReasonCode::KfdMissing]
    );
    let acestep = variant(&inventory, "acestep", "rocm");
    assert_eq!(acestep.state, Availability::Unavailable);
    assert_eq!(report.devices.vulkan.state, Availability::Unknown);
    assert_eq!(report.devices.vulkan.basis, "icd_manifests_unverified");
    let vulkan = variant(&inventory, "llamacpp", "vulkan");
    assert_eq!(vulkan.state, Availability::Unknown);
    assert_eq!(codes(vulkan), [ReasonCode::VulkanUnverified]);
}

#[test]
fn intel_igpu_and_npu_are_classified() {
    let tree = Tree::new();
    tree.file(
        "proc/cpuinfo",
        cpuinfo(
            "Intel(R) Core(TM) Ultra 7 155H",
            "GenuineIntel",
            16,
            1,
            "sse4_2 avx avx2 fma f16c avx_vnni",
        ),
    )
    .file("proc/meminfo", MEMINFO)
    .file("sys/class/drm/card0/device/vendor", "0x8086\n")
    .file("sys/class/drm/card0/device/device", "0x7d55\n")
    .file(
        "sys/class/drm/card0/device/uevent",
        "DRIVER=i915\nPCI_SLOT_NAME=0000:00:02.0\n",
    )
    .dir("sys/class/drm/card0/device/drm/renderD128")
    .file("dev/dri/renderD128", "")
    .file("sys/class/drm/card1/device/vendor", "0x8086\n")
    .file(
        "sys/class/drm/card1/device/uevent",
        "DRIVER=xe\nPCI_SLOT_NAME=0000:03:00.0\n",
    )
    .file("sys/class/accel/accel0/device/uevent", "DRIVER=intel_vpu\n")
    .file("sys/class/accel/accel0/device/vendor", "0x8086\n")
    .file("sys/class/accel/accel0/device/device", "0x7d1d\n")
    .file("dev/accel/accel0", "");
    let report = report(&tree);
    assert_eq!(
        report.devices.cpu.as_ref().unwrap().vendor.as_deref(),
        Some("intel")
    );
    assert_eq!(report.devices.gpus.len(), 2);
    assert!(
        report
            .devices
            .gpus
            .iter()
            .all(|gpu| gpu.vendor == GpuVendor::Intel)
    );
    assert_eq!(report.devices.gpus[0].integrated, Some(true));
    assert_eq!(report.devices.gpus[0].driver.as_deref(), Some("i915"));
    assert_eq!(report.devices.gpus[1].integrated, Some(false));
    assert_eq!(report.devices.gpus[1].render_node, None);
    let npu = &report.devices.npus[0];
    assert_eq!(
        (npu.vendor, npu.name.as_str(), npu.family),
        (NpuVendor::Intel, "Intel NPU", None)
    );
    let inventory = inventory(&report);
    assert_eq!(
        codes(variant(&inventory, "flm", "npu")),
        [ReasonCode::NoAmdNpu]
    );
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "rocm")),
        [ReasonCode::NoAmdGpu]
    );
    assert_eq!(recommended(&inventory, "llamacpp"), Some("cpu"));
}

#[test]
fn malformed_and_oversized_inputs_stay_unknown() {
    let tree = Tree::new();
    tree.file("proc/cpuinfo", vec![b'x'; super::sysfs::CPUINFO_BYTES + 1])
        .file("proc/meminfo", "MemTotal: lots\nMemAvailable: 12 MB\n")
        .file("sys/class/drm/card0/device/vendor", "vendor\n")
        .file("sys/class/drm/card1/device/vendor", "0x1002\n")
        .file("sys/class/drm/card1/device/mem_info_vram_total", "-5\n")
        .file("sys/class/drm/card1/device/uevent", vec![0xff, 0xfe, b'\n'])
        .file("sys/class/drm/card1/device/board_info", "type : dGPU\n")
        .file(
            "sys/class/kfd/kfd/topology/nodes/1/properties",
            "gfx_target_version 99999999\ndrm_render_minor 128\n",
        )
        .file("sys/class/drm/card2/device/vendor", "0x10de\n")
        .command(
            "nvidia-smi",
            "garbage line\n0, GPU-x, NVIDIA Test\u{7}, [N/A], [N/A], 99.x, [Not Supported], bogus\n",
        )
        .command("vulkaninfo", "Devices:\n========\n");
    let report = report(&tree);
    let cpu = report.devices.cpu.as_ref().unwrap();
    assert_eq!(
        (cpu.name.as_ref(), cpu.physical_cores, cpu.logical_threads),
        (None, None, None)
    );
    assert_eq!(report.devices.memory.total_bytes, None);
    assert_eq!(report.devices.memory.available_bytes, None);
    let amd = report
        .devices
        .gpus
        .iter()
        .find(|gpu| gpu.vendor == GpuVendor::Amd)
        .unwrap();
    assert_eq!(amd.vram_total_bytes, None);
    assert_eq!(amd.vram_free_bytes, None);
    assert_eq!(amd.gfx_target, None);
    assert_eq!(amd.integrated, Some(false));
    let nvidia: Vec<_> = report
        .devices
        .gpus
        .iter()
        .filter(|gpu| gpu.vendor == GpuVendor::Nvidia)
        .collect();
    assert_eq!(nvidia.len(), 2);
    let parsed = nvidia.iter().find(|gpu| gpu.uuid.is_some()).unwrap();
    assert_eq!(parsed.name, None);
    assert_eq!(parsed.vram_total_bytes, None);
    assert_eq!(parsed.compute_capability, None);
    assert_eq!(parsed.family, None);
    assert_eq!(parsed.driver_version, None);
    assert_eq!(report.devices.vulkan.state, Availability::Unavailable);
    assert_eq!(report.devices.vulkan.basis, "vulkaninfo_no_devices");
    let issues: Vec<_> = report
        .issues
        .iter()
        .map(|issue| (issue.source.as_str(), issue.code))
        .collect();
    assert!(
        issues.contains(&("/proc/cpuinfo", "read_limit")),
        "{issues:?}"
    );
    assert!(issues.contains(&("/proc/meminfo", "invalid_metadata")));
    assert!(issues.contains(&("nvidia-smi", "invalid_output")));
    assert!(issues.contains(&("/sys/class/drm/card1/device/uevent", "invalid_encoding")));
    assert!(issues.contains(&(
        "/sys/class/kfd/kfd/topology/nodes/1/properties",
        "invalid_metadata"
    )));
    let inventory = inventory(&report);
    let cuda = variant(&inventory, "llamacpp", "cuda");
    assert_eq!(cuda.state, Availability::Unknown);
    assert_eq!(codes(cuda), [ReasonCode::NvidiaComputeUnknown]);
    assert_eq!(
        variant(&inventory, "acestep", "cuda").state,
        Availability::Available
    );
}

#[test]
fn oversized_and_failed_helper_output_is_not_trusted() {
    let tree = rtx_host();
    tree.command("nvidia-smi", &NVIDIA_SMI.repeat(2000));
    tree.file("commands/vulkaninfo.status", "1");
    let report = report(&tree);
    assert_eq!(report.devices.cuda.nvidia_smi, "output_limit");
    let gpu = &report.devices.gpus[0];
    assert_eq!(gpu.sources, ["drm"]);
    assert_eq!(gpu.name, None);
    assert_eq!(gpu.vram_total_bytes, None);
    assert_eq!(report.devices.vulkan.state, Availability::Unknown);
    let inventory = inventory(&report);
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "cuda")),
        [
            ReasonCode::NvidiaComputeUnknown,
            ReasonCode::NvidiaDriverUnverified
        ]
    );
    let acestep = variant(&inventory, "acestep", "cuda");
    assert_eq!(acestep.state, Availability::Unknown);
    assert_eq!(codes(acestep), [ReasonCode::NvidiaDriverUnverified]);
    assert_eq!(recommended(&inventory, "llamacpp"), Some("cpu"));
}

#[test]
fn command_timeout_keeps_nvidia_presence_unverified() {
    let tree = Tree::new();
    tree.file(
        "proc/cpuinfo",
        cpuinfo("Test CPU", "AuthenticAMD", 4, 1, ZEN2_FLAGS),
    )
    .file("commands/nvidia-smi.timeout", "");
    let report = report(&tree);
    assert_eq!(report.devices.cuda.nvidia_smi, "timed_out");
    assert!(report.devices.gpus.is_empty());
    assert_eq!(report.probes[0].outcome, "timed_out");
    assert_eq!(report.devices.memory.total_bytes, None);
    assert_eq!(report.devices.memory.source, "unavailable");
    let inventory = inventory(&report);
    let cuda = variant(&inventory, "llamacpp", "cuda");
    assert_eq!(cuda.state, Availability::Unknown);
    assert_eq!(codes(cuda), [ReasonCode::NvidiaDriverUnverified]);
}

#[test]
fn expired_deadline_marks_detection_incomplete() {
    let tree = rtx_host();
    let report = detect(&tree.probe(), Instant::now() - Duration::from_millis(1));
    assert_eq!(report.host.detection, Detection::Incomplete);
    assert!(report.devices.gpus.is_empty());
    assert!(report.probes.is_empty());
    assert_eq!(report.devices.memory.total_bytes, None);
    assert!(report.issues.contains(&Issue {
        source: "detection".into(),
        code: "deadline"
    }));
    let inventory = inventory(&report);
    let cuda = variant(&inventory, "llamacpp", "cuda");
    assert_eq!(cuda.state, Availability::Unknown);
    assert!(codes(cuda).contains(&ReasonCode::HostDetectionIncomplete));
    assert_eq!(
        codes(variant(&inventory, "llamacpp", "metal")),
        [ReasonCode::OsUnsupported]
    );
    assert_recommendations_are_core(&inventory);
}

#[test]
fn unsupported_platforms_report_unknown_without_reading() {
    let tree = rtx_host();
    for os in ["windows", "macos"] {
        let probe = FixtureProbe {
            root: tree.0.path().into(),
            os,
            architecture: "aarch64",
            supported: false,
        };
        let report = detect(&probe, Instant::now() + Duration::from_secs(5));
        assert_eq!(report.host.detection, Detection::Unsupported);
        assert_eq!(report.host.cpu_family, "arm64");
        assert!(report.devices.cpu.is_none() && report.devices.gpus.is_empty());
        assert_eq!(report.devices.memory.total_bytes, None);
        assert_eq!(report.devices.vulkan.state, Availability::Unknown);
        assert!(report.probes.is_empty());
        let inventory = inventory(&report);
        assert!(inventory.recommendations.values().all(Option::is_none));
        let metal = variant(&inventory, "llamacpp", "metal");
        let ryzenai = variant(&inventory, "ryzenai-llm", "npu");
        if os == "macos" {
            assert_eq!(codes(metal), [ReasonCode::HostDetectionUnsupported]);
            assert_eq!(codes(ryzenai), [ReasonCode::OsUnsupported]);
        } else {
            assert_eq!(codes(metal), [ReasonCode::OsUnsupported]);
            assert_eq!(ryzenai.state, Availability::Unknown);
            assert_eq!(codes(ryzenai), [ReasonCode::HostDetectionUnsupported]);
        }
        assert_eq!(
            codes(variant(&inventory, "asr4all", "cpu")),
            [ReasonCode::OsUnsupported]
        );
    }
}

#[test]
fn aarch64_cpu_uses_features_and_sysfs_topology() {
    let tree = Tree::new();
    let mut text = String::new();
    for processor in 0..4 {
        text.push_str(&format!(
            "processor\t: {processor}\nBogoMIPS\t: 50.00\nFeatures\t: fp asimd evtstrm aes asimdhp asimddp sve i8mm bf16\nCPU implementer\t: 0x41\n\n"
        ));
        tree.file(
            &format!("sys/devices/system/cpu/cpu{processor}/topology/physical_package_id"),
            "0\n",
        )
        .file(
            &format!("sys/devices/system/cpu/cpu{processor}/topology/core_id"),
            format!("{processor}\n"),
        );
    }
    tree.file("proc/cpuinfo", text)
        .dir("sys/devices/system/cpu/cpufreq");
    let probe = FixtureProbe {
        root: tree.0.path().into(),
        os: "linux",
        architecture: "aarch64",
        supported: true,
    };
    let report = detect(&probe, Instant::now() + Duration::from_secs(5));
    let cpu = report.devices.cpu.as_ref().unwrap();
    assert_eq!(cpu.name, None);
    assert_eq!(
        (cpu.physical_cores, cpu.logical_threads),
        (Some(4), Some(4))
    );
    assert_eq!(cpu.isa, ["neon", "fp16", "dotprod", "i8mm", "bf16", "sve"]);
    let inventory = inventory(&report);
    assert_eq!(
        variant(&inventory, "llamacpp", "cpu").state,
        Availability::Available
    );
    assert_eq!(
        codes(variant(&inventory, "whispercpp", "cpu")),
        [ReasonCode::ArchitectureUnsupported]
    );
    assert_eq!(
        variant(&inventory, "whispercpp", "cpu").reasons[0].message,
        "Requires a x86_64 CPU; this host is arm64."
    );
}

#[test]
fn amx_flags_are_reported() {
    let info = super::sysfs::parse_cpuinfo(&cpuinfo(
        "Intel(R) Xeon(R) Platinum 8480+",
        "GenuineIntel",
        2,
        1,
        "avx avx2 avx512f amx_bf16 amx_tile amx_int8",
    ));
    assert_eq!(
        info.isa,
        ["avx", "avx2", "avx512f", "amx", "amx_int8", "amx_bf16"]
    );
}

#[test]
fn identifiers_parse_conservatively() {
    use super::sysfs::{gfx_name, normalize_pci_slot, sm_family};
    assert_eq!(gfx_name(110500).as_deref(), Some("gfx1150"));
    assert_eq!(gfx_name(110501).as_deref(), Some("gfx1151"));
    assert_eq!(gfx_name(90010).as_deref(), Some("gfx90a"));
    assert_eq!(gfx_name(90402).as_deref(), Some("gfx942"));
    assert_eq!(gfx_name(120001).as_deref(), Some("gfx1201"));
    assert_eq!(gfx_name(100300).as_deref(), Some("gfx1030"));
    assert_eq!(gfx_name(0), None);
    assert_eq!(gfx_name(99999999), None);
    assert_eq!(gfx_name(101700), None);
    assert_eq!(sm_family("12.0").as_deref(), Some("sm_120"));
    assert_eq!(sm_family("8.6").as_deref(), Some("sm_86"));
    assert_eq!(sm_family("x.0"), None);
    assert_eq!(sm_family("12.10"), None);
    assert_eq!(
        normalize_pci_slot("00000000:09:00.0").as_deref(),
        Some("0000:09:00.0")
    );
    assert_eq!(
        normalize_pci_slot("0000:C4:00.0").as_deref(),
        Some("0000:c4:00.0")
    );
    assert_eq!(normalize_pci_slot("09:00.0"), None);
    assert_eq!(normalize_pci_slot("0000:09:00"), None);
}

#[test]
fn every_native_support_row_is_projected_once_per_backend() {
    let catalog = Catalog::from_seed().unwrap();
    let inventory = inventory(&report(&rtx_host()));
    let expected: usize = catalog
        .native_recipe_migrations()
        .iter()
        .map(|migration| migration.native_backend_labels.len())
        .sum();
    assert_eq!(inventory.backends.len(), expected);
    for migration in catalog.native_recipe_migrations() {
        for label in &migration.native_backend_labels {
            let variant = variant(&inventory, &migration.legacy_recipe, label);
            assert_ne!(variant.tier, "unknown");
            assert!(!variant.device_declaration.is_empty());
            if variant.state != Availability::Available {
                assert!(
                    !variant.reasons.is_empty(),
                    "{}:{label}",
                    migration.legacy_recipe
                );
            } else {
                assert!(variant.reasons.is_empty());
                assert!(!variant.devices.is_empty());
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn bounded_runner_enforces_timeout_limit_and_absence() {
    let started = Instant::now();
    assert_eq!(
        run_bounded("sleep", &["5"], Duration::from_millis(200), 1024),
        CommandOutcome::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let started = Instant::now();
    assert_eq!(
        run_bounded(
            "sh",
            &["-c", "sleep 5 & echo started"],
            Duration::from_millis(300),
            1024
        ),
        CommandOutcome::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        run_bounded("yes", &[], Duration::from_secs(5), 4096),
        CommandOutcome::OutputLimit
    );
    assert_eq!(
        run_bounded(
            "juiced-missing-helper-for-tests",
            &[],
            Duration::from_secs(1),
            64
        ),
        CommandOutcome::NotFound
    );
    assert_eq!(
        run_bounded("sh", &["-c", "echo ok; exit 3"], Duration::from_secs(5), 64),
        CommandOutcome::Failed(Some(3))
    );
    assert_eq!(
        run_bounded("sh", &["-c", "printf 'a,b\\n'"], Duration::from_secs(5), 64),
        CommandOutcome::Output("a,b\n".into())
    );
    assert_eq!(
        run_bounded("sh", &["-c", "printf '\\377'"], Duration::from_secs(5), 64),
        CommandOutcome::InvalidOutput
    );
}

struct CountingProbe {
    inner: FixtureProbe,
    runs: AtomicUsize,
    stall: Duration,
}

impl HostProbe for CountingProbe {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn run(
        &self,
        program: &'static str,
        args: &[&str],
        timeout: Duration,
        limit: usize,
    ) -> CommandOutcome {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.inner.run(program, args, timeout, limit)
    }

    fn online_threads(&self) -> Option<u64> {
        std::thread::sleep(self.stall);
        None
    }

    fn device_access(&self, device: &str) -> DeviceAccess {
        self.inner.device_access(device)
    }
}

#[tokio::test]
async fn detector_caches_within_ttl_and_refreshes_after_it() {
    let tree = rtx_host();
    let probe = Arc::new(CountingProbe {
        inner: tree.probe(),
        runs: AtomicUsize::new(0),
        stall: Duration::ZERO,
    });
    let detector = Detector::new(
        probe.clone(),
        Duration::from_secs(60),
        Duration::from_secs(5),
    );
    let first = detector.snapshot().await.unwrap();
    let second = detector.snapshot().await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(probe.runs.load(Ordering::SeqCst), 2);
    let expiring = Detector::new(probe.clone(), Duration::ZERO, Duration::from_secs(5));
    expiring.snapshot().await.unwrap();
    expiring.snapshot().await.unwrap();
    assert_eq!(probe.runs.load(Ordering::SeqCst), 6);
    let catalog = Catalog::from_seed().unwrap();
    let document = serde_json::to_value(SystemInfo::new(&first, detector.ttl(), &catalog)).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["cache_ttl_ms"], 60000);
    assert_eq!(document["devices"]["gpus"][0]["family"], "sm_120");
    assert_eq!(document["recommendations"]["llamacpp"], "cuda");
    assert_eq!(document["backends"].as_array().unwrap().len(), 40);
    assert!(document["backends"][0]["installed"].is_null());
}

#[tokio::test]
async fn detector_deadline_returns_error_without_caching() {
    let tree = rtx_host();
    let probe = Arc::new(CountingProbe {
        inner: tree.probe(),
        runs: AtomicUsize::new(0),
        stall: Duration::from_millis(2500),
    });
    let detector = Detector::new(probe, Duration::from_secs(60), Duration::from_millis(1));
    let started = Instant::now();
    assert_eq!(
        detector.snapshot().await.err(),
        Some(DetectionError::Deadline)
    );
    assert!(started.elapsed() < Duration::from_millis(2400));
    assert!(detector.current.lock().await.is_none());
}
