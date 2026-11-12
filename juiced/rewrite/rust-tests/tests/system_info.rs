mod support;

use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::path::Path;
use support::{ADMIN_KEY, CLIENT_KEY, PREFIXES, Server};

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn strix_point(root: &Path, kfd: bool) {
    let mut cpuinfo = String::new();
    for processor in 0..24 {
        cpuinfo.push_str(&format!(
            "processor\t: {processor}\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen AI 9 HX 370 w/ Radeon 890M\nphysical id\t: 0\ncore id\t\t: {}\nflags\t\t: avx avx2 fma f16c avx512f avx512_bf16\n\n",
            processor % 12
        ));
    }
    write(root, "proc/cpuinfo", &cpuinfo);
    write(
        root,
        "proc/meminfo",
        "MemTotal:       98304000 kB\nMemAvailable:   81920000 kB\n",
    );
    let card = "sys/class/drm/card1/device";
    write(root, &format!("{card}/vendor"), "0x1002\n");
    write(root, &format!("{card}/device"), "0x150e\n");
    write(
        root,
        &format!("{card}/uevent"),
        "DRIVER=amdgpu\nPCI_SLOT_NAME=0000:c4:00.0\n",
    );
    write(root, &format!("{card}/mem_info_vram_total"), "536870912\n");
    std::fs::create_dir_all(root.join(card).join("drm/renderD128")).unwrap();
    write(root, "dev/dri/renderD128", "");
    write(
        root,
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "gfx_target_version 110500\ndrm_render_minor 128\n",
    );
    write(
        root,
        "sys/class/accel/accel0/device/uevent",
        "DRIVER=amdxdna\nPCI_SLOT_NAME=0000:c5:00.1\n",
    );
    write(root, "sys/class/accel/accel0/device/device", "0x17f0\n");
    write(root, "dev/accel/accel0", "");
    write(root, "commands/vulkaninfo.timeout", "");
    if kfd {
        write(root, "dev/kfd", "");
    }
}

fn variant<'a>(document: &'a Value, recipe: &str, backend: &str) -> &'a Value {
    document["backends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["recipe"] == recipe && variant["backend"] == backend)
        .unwrap_or_else(|| panic!("missing {recipe}:{backend}"))
}

async fn fixture_server(kfd: bool) -> (Server, tempfile::TempDir) {
    let hardware = tempfile::tempdir().unwrap();
    strix_point(hardware.path(), kfd);
    let mut server = Server::prepare(true, |_| {});
    server.set_environment("JUICED_TEST_HARDWARE_ROOT", hardware.path());
    server.start().await;
    (server, hardware)
}

#[tokio::test]
async fn system_info_reports_fixture_hardware_under_every_prefix_with_operator_authority() {
    let (server, _hardware) = fixture_server(true).await;
    for prefix in PREFIXES {
        let path = format!("{prefix}/system-info");
        for key in [None, Some("invalid-key"), Some(CLIENT_KEY)] {
            let (status, body) = server.json(Method::GET, &path, None, key).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "GET {path} {key:?}");
            if key == Some(CLIENT_KEY) {
                assert_eq!(body["error"]["code"], "operator_required");
            }
        }
        let response = server
            .request(Method::POST, &path, None, Some(ADMIN_KEY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let head = server
            .request(Method::HEAD, &path, None, Some(ADMIN_KEY))
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head.bytes().await.unwrap().is_empty());
        let response = server
            .request(Method::GET, &path, None, Some(ADMIN_KEY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let document: Value = response.json().await.unwrap();
        assert_eq!(document["schema_version"], 1);
        assert_eq!(document["host"]["detection"], "supported");
        let devices = &document["devices"];
        assert_eq!(devices["cpu"]["physical_cores"], 12);
        assert_eq!(devices["cpu"]["logical_threads"], 24);
        assert_eq!(devices["memory"]["total_bytes"], 98304000u64 * 1024);
        let gpu = &devices["gpus"][0];
        assert_eq!(gpu["vendor"], "amd");
        assert_eq!(gpu["gfx_target"], "gfx1150");
        assert_eq!(gpu["integrated"], true);
        assert_eq!(gpu["vram_total_bytes"], 536870912u64);
        assert!(gpu["vram_free_bytes"].is_null());
        assert_eq!(devices["npus"][0]["family"], "XDNA2");
        assert_eq!(devices["npus"][0]["driver"], "amdxdna");
        assert_eq!(devices["rocm"]["kfd"], "read_write");
        assert_eq!(devices["cuda"]["nvidia_smi"], "not_found");
        assert_eq!(devices["vulkan"]["state"], "unknown");
        let probes: Vec<_> = document["probes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|probe| (probe["tool"].clone(), probe["outcome"].clone()))
            .collect();
        assert_eq!(
            probes,
            [
                ("nvidia-smi".into(), "not_found".into()),
                ("vulkaninfo".into(), "timed_out".into())
            ]
        );
        assert_eq!(document["recommendations"]["llamacpp"], "rocm");
        assert_eq!(document["recommendations"]["flm"], "npu");
        assert_eq!(document["recommendations"]["vllm"], Value::Null);
        let hybrid = variant(&document, "llamacpp", "hybrid-rocm");
        assert_eq!(hybrid["state"], "available");
        assert_eq!(hybrid["recommended"], false);
        assert_eq!(hybrid["implementation_id"], "llama-xdna-hybrid");
        assert_eq!(
            variant(&document, "llamacpp", "cuda")["state"],
            "unavailable"
        );
        assert_eq!(
            variant(&document, "llamacpp", "cuda")["reasons"][0]["code"],
            "no_nvidia_gpu"
        );
        assert_eq!(
            variant(&document, "llamacpp", "vulkan")["reasons"][0]["code"],
            "vulkan_unverified"
        );
        for variant in document["backends"].as_array().unwrap() {
            assert!(variant["installed"].is_null());
            if variant["recommended"] == true {
                assert_eq!(variant["tier"], "core");
            }
        }
    }
}

#[tokio::test]
async fn system_info_without_kfd_refuses_rocm_and_preserves_service_state() {
    let (server, _hardware) = fixture_server(false).await;
    let before = server.get("juiced/state").await;
    let document = server.get("system-info").await;
    let rocm = variant(&document, "llamacpp", "rocm");
    assert_eq!(rocm["state"], "unavailable");
    assert_eq!(rocm["reasons"][0]["code"], "kfd_missing");
    assert_eq!(document["devices"]["gpus"][0]["gfx_target"], "gfx1150");
    assert_eq!(document["recommendations"]["llamacpp"], "cpu");
    assert_eq!(server.get("juiced/state").await, before);
}

#[tokio::test]
async fn system_info_observes_the_actual_host() {
    let server = Server::new(false).await;
    let (status, document) = server
        .json(Method::GET, "/v1/system-info", None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["schema_version"], 1);
    assert!(document["backends"].as_array().unwrap().len() >= 40);
    if cfg!(target_os = "linux") {
        assert_ne!(document["host"]["detection"], "unsupported");
        let threads = document["devices"]["cpu"]["logical_threads"].as_u64();
        assert!(threads.is_some_and(|threads| threads > 0));
        assert!(
            document["devices"]["memory"]["total_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
        );
    } else {
        assert_eq!(document["host"]["detection"], "unsupported");
        assert!(document["devices"]["memory"]["total_bytes"].is_null());
    }
    let (_, again) = server
        .json(Method::GET, "/v1/system-info", None, None)
        .await;
    assert_eq!(
        again["observed_at_unix_ms"],
        document["observed_at_unix_ms"]
    );
}
