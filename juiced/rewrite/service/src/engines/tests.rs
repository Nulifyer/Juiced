use super::*;
use crate::hardware::tests::{cpu_only_host, report, rtx_host, strix_host};
use archive::{ArchiveError, LIMITS, Limits, describe, extract};
use catalog::{ArchiveFormat, Unresolved, resolve};
use models::{FormatError, ModelFormat};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64},
};

fn inventory(host: &HostReport) -> Inventory {
    crate::hardware::project(&crate::catalog::Catalog::from_seed().unwrap(), host)
}

#[test]
fn rtx_5080_resolves_the_sm_120_cuda_build_and_ggml_cpu_vulkan_builds() {
    let host = report(&rtx_host());
    let cuda = resolve(Engine::LlamaCpp, "cuda", &host).unwrap();
    assert_eq!(cuda.repository, "lemonade-sdk/llama.cpp");
    assert_eq!(cuda.tag, "b11395");
    assert_eq!(cuda.gpu_target, Some("sm_120"));
    assert_eq!(cuda.asset, "llama-b11395-ubuntu-cuda-sm_120-x64.tar.xz");
    assert_eq!(cuda.format, ArchiveFormat::TarXz);
    assert_eq!(
        cuda.sha256,
        Some("cffd05da1d68793e2131224af2ed437e2400d5ee0cc20c7cd10804e977092457")
    );
    assert_eq!(
        cuda.download_url(GITHUB),
        "https://github.com/lemonade-sdk/llama.cpp/releases/download/b11395/llama-b11395-ubuntu-cuda-sm_120-x64.tar.xz"
    );
    let cpu = resolve(Engine::LlamaCpp, "cpu", &host).unwrap();
    assert_eq!(cpu.repository, "ggml-org/llama.cpp");
    assert_eq!(cpu.asset, "llama-b12032-bin-ubuntu-x64.tar.gz");
    assert_eq!(
        resolve(Engine::LlamaCpp, "vulkan", &host).unwrap().asset,
        "llama-b12032-bin-ubuntu-vulkan-x64.tar.gz"
    );
    assert_eq!(
        resolve(Engine::LlamaCpp, "rocm", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    assert_eq!(
        resolve(Engine::WhisperCpp, "cpu", &host).unwrap().asset,
        "whisper-v1.8.4-linux-cpu-x86_64.tar.gz"
    );
    assert_eq!(
        resolve(Engine::LlamaCpp, "metal", &host).unwrap_err(),
        Unresolved::BackendUnknown
    );
    assert_eq!(
        inventory(&host).recommendations["llamacpp"].as_deref(),
        Some("cuda")
    );
}

#[test]
fn strix_point_resolves_gfx1150_rocm_builds_and_selects_hybrid_only_on_request() {
    let host = report(&strix_host(true));
    let rocm = resolve(Engine::LlamaCpp, "rocm", &host).unwrap();
    assert_eq!(rocm.repository, "lemonade-sdk/llama.cpp");
    assert_eq!(rocm.asset, "llama-b11580-bin-ubuntu-rocm-10.1-x64.tar.gz");
    assert_eq!(rocm.format, ArchiveFormat::TarGz);
    assert_eq!(
        resolve(Engine::WhisperCpp, "rocm", &host).unwrap().asset,
        "whisper-v1.8.4-linux-rocm-gfx1150.tar.gz"
    );
    assert_eq!(
        resolve(Engine::LlamaCpp, "cuda", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    let hybrid = resolve(Engine::LlamaCpp, "hybrid-rocm", &host).unwrap();
    assert_eq!(hybrid.repository, "Nulifyer/llama-xdna-hybrid");
    assert_eq!(hybrid.tag, "rocm-v0.3.2");
    assert_eq!(
        hybrid.asset,
        "llama-xdna-hybrid-linux-rocm-gfx1150-x86_64.tar.gz"
    );
    assert_eq!(hybrid.gpu_target, Some("gfx1150"));
    assert_eq!(
        hybrid.sha256,
        Some("f9d2825c59a2c7a755c36a6482a20525ebf16521441e1b3b4860c709c1016a1b")
    );
    let flm = resolve(Engine::FastFlowLm, "npu", &host).unwrap();
    assert_eq!(flm.repository, "ROCm/FastFlowLM");
    assert_eq!(flm.tag, "v1.0.12");
    assert_eq!(flm.asset, "fastflowlm_1.0.12_linux.tar.gz");
    assert_eq!(flm.engine.entrypoint(), "flm-real");
    assert_eq!(
        flm.sha256,
        Some("626fbee5e5206a215a8c2c4d3d961055cbf7289d257d36599e9fef38f2f555a8")
    );
    let inventory = inventory(&host);
    assert_eq!(
        inventory.recommendations["llamacpp"].as_deref(),
        Some("rocm")
    );
    assert_eq!(inventory.recommendations["flm"].as_deref(), Some("npu"));
    assert_eq!(
        choose(
            Engine::LlamaCpp,
            &inventory,
            &host,
            &[],
            Some("hybrid-rocm")
        ),
        Err(ChoiceError::NotInstalled {
            backend: "hybrid-rocm".into(),
            gpu_target: Some("gfx1150".into()),
            installable: true,
        })
    );
    let mut no_npu = host.clone();
    no_npu.devices.npus.clear();
    assert_eq!(
        resolve(Engine::LlamaCpp, "hybrid-rocm", &no_npu).unwrap_err(),
        Unresolved::NoMatchingNpu
    );
    assert_eq!(
        choose(
            Engine::LlamaCpp,
            &super::tests::inventory(&no_npu),
            &no_npu,
            &[],
            Some("hybrid-rocm")
        ),
        Err(ChoiceError::NotSelectable("hybrid-rocm".into()))
    );
    assert_eq!(
        choose(Engine::LlamaCpp, &inventory, &host, &[], Some("system")),
        Err(ChoiceError::NotInstalled {
            backend: "vulkan".into(),
            gpu_target: None,
            installable: true,
        })
    );
    assert_eq!(catalog::rocm_asset_family("gfx1201"), "gfx120X");
    assert_eq!(catalog::rocm_asset_family("gfx1103"), "gfx110X");
    assert_eq!(catalog::rocm_asset_family("gfx90a"), "gfx90a");
}

#[test]
fn cpu_only_hosts_resolve_only_target_free_builds() {
    let host = report(&cpu_only_host());
    assert_eq!(
        resolve(Engine::LlamaCpp, "cuda", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    assert!(resolve(Engine::LlamaCpp, "cpu", &host).is_ok());
    let inventory = inventory(&host);
    assert_eq!(
        inventory.recommendations["llamacpp"].as_deref(),
        Some("cpu")
    );
    let rows = installable(&inventory, &host, &[]);
    let cuda = rows
        .iter()
        .find(|row| row.engine == Engine::LlamaCpp && row.backend == "cuda")
        .unwrap();
    assert!(cuda.build.is_none());
    assert_eq!(cuda.unresolved, Some(Unresolved::NoMatchingGpu));
    assert_eq!(cuda.availability, Some(Availability::Unavailable));
    let cpu = rows
        .iter()
        .find(|row| row.engine == Engine::LlamaCpp && row.backend == "cpu")
        .unwrap();
    assert!(cpu.recommended);
    let mut aarch64 = host.clone();
    aarch64.host.architecture = "aarch64";
    assert_eq!(
        resolve(Engine::LlamaCpp, "cpu", &aarch64).unwrap_err(),
        Unresolved::HostUnsupported
    );
}

fn package(id: &str, backend: &str, target: Option<&str>, tag: &str) -> Installed {
    Installed {
        package: id.into(),
        engine: Some(Engine::LlamaCpp),
        backend: Some(backend.into()),
        gpu_target: target.map(str::to_owned),
        tag: Some(tag.into()),
        installed: true,
        declared_host_match: true,
    }
}

#[test]
fn automatic_choice_requires_the_recommended_installed_package_for_the_detected_target() {
    let host = report(&rtx_host());
    let inventory = inventory(&host);
    let cpu = package("package-cpu", "cpu", None, "b12032");
    assert_eq!(
        choose(
            Engine::LlamaCpp,
            &inventory,
            &host,
            std::slice::from_ref(&cpu),
            None
        ),
        Err(ChoiceError::NotInstalled {
            backend: "cuda".into(),
            gpu_target: Some("sm_120".into()),
            installable: true,
        })
    );
    let wrong_target = package("package-sm86", "cuda", Some("sm_86"), "b11395");
    assert!(matches!(
        choose(
            Engine::LlamaCpp,
            &inventory,
            &host,
            std::slice::from_ref(&wrong_target),
            None
        ),
        Err(ChoiceError::NotInstalled { .. })
    ));
    let older = package("package-old", "cuda", Some("sm_120"), "b10000");
    let current = package("package-new", "cuda", Some("sm_120"), "b11395");
    let choice = choose(
        Engine::LlamaCpp,
        &inventory,
        &host,
        &[current.clone(), older, cpu.clone(), wrong_target],
        None,
    )
    .unwrap();
    assert_eq!(choice.backend, "cuda");
    assert_eq!(choice.package, "package-new");
    assert!(choice.gpu);
    assert!(choice.reason.contains("recommended"));
    let explicit = choose(
        Engine::LlamaCpp,
        &inventory,
        &host,
        &[cpu, current],
        Some("cpu"),
    )
    .unwrap();
    assert_eq!(explicit.package, "package-cpu");
    assert!(!explicit.gpu);
    let mut unmatched = package("package-foreign", "cuda", Some("sm_120"), "b11395");
    unmatched.declared_host_match = false;
    assert!(choose(Engine::LlamaCpp, &inventory, &host, &[unmatched], None).is_err());
}

#[test]
fn installed_packages_read_release_provenance_from_the_runtime_inventory() {
    let inventory = serde_json::json!({"generation":3,"packages":[
        {"id":"package-a","phase":"installed","implementation_id":"llama-cpp","declared_host_match":true,
         "provenance":{"release":{"engine":"llama.cpp","backend":"cuda","gpu_target":"sm_120","tag":"b11395"}}},
        {"id":"package-b","phase":"staging","implementation_id":"whisper-cpp","declared_host_match":true,
         "provenance":{}}
    ]});
    let packages = installed_packages(&inventory);
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0].backend.as_deref(), Some("cuda"));
    assert_eq!(packages[0].gpu_target.as_deref(), Some("sm_120"));
    assert!(packages[0].installed);
    assert_eq!(packages[1].engine, Some(Engine::WhisperCpp));
    assert_eq!(packages[1].backend, None);
    assert!(!packages[1].installed);
}

#[test]
fn every_catalog_build_is_pinned_unique_and_named_for_its_backend() {
    let mut assets = std::collections::BTreeSet::new();
    for build in catalog::BUILDS {
        assert!(assets.insert(build.asset), "{}", build.asset);
        let sha = build.sha256.unwrap();
        assert!(sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(build.archive_bytes > 0);
        if let Some(target) = build.gpu_target {
            assert!(
                build.asset.contains(target)
                    || build
                        .runtime
                        .is_some_and(|runtime| runtime.gpu_target == target
                            && runtime.asset.contains(target)),
                "{}",
                build.asset
            );
        }
        let suffix = match build.format {
            ArchiveFormat::TarGz => ".tar.gz",
            ArchiveFormat::TarXz => ".tar.xz",
            ArchiveFormat::Zip => ".zip",
        };
        assert!(build.asset.ends_with(suffix));
    }
}

#[test]
fn release_provenance_is_bounded_label_text_with_a_hex_digest() {
    let release = crate::runtime_packages::ReleaseSource {
        engine: "llama.cpp".into(),
        backend: "cuda".into(),
        gpu_target: Some("sm_120".into()),
        repository: "lemonade-sdk/llama.cpp".into(),
        tag: "b11395".into(),
        asset: "llama-b11395-ubuntu-cuda-sm_120-x64.tar.xz".into(),
        archive_sha256: "c".repeat(64),
        archive_bytes: 607784448,
        digest_source: "pinned".into(),
        runtime: None,
    };
    assert!(release.valid());
    for change in [
        |release: &mut crate::runtime_packages::ReleaseSource| {
            release.archive_sha256 = "C".repeat(64)
        },
        |release: &mut crate::runtime_packages::ReleaseSource| release.asset = "a\nb".into(),
        |release: &mut crate::runtime_packages::ReleaseSource| release.tag = String::new(),
        |release: &mut crate::runtime_packages::ReleaseSource| release.repository = "x".repeat(257),
    ] {
        let mut changed = release.clone();
        change(&mut changed);
        assert!(!changed.valid());
    }
}

#[derive(Default)]
struct TarBuilder(Vec<u8>);

impl TarBuilder {
    fn raw(&mut self, name: &[u8], kind: tar::EntryType, mode: u32, link: &[u8], data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        {
            let gnu = header.as_gnu_mut().unwrap();
            gnu.name[..name.len()].copy_from_slice(name);
            gnu.linkname[..link.len()].copy_from_slice(link);
        }
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        header.set_cksum();
        self.0.extend_from_slice(header.as_bytes());
        self.0.extend_from_slice(data);
        let padding = (512 - data.len() % 512) % 512;
        self.0.extend(std::iter::repeat_n(0, padding));
    }
    fn file(&mut self, name: &str, mode: u32, data: &[u8]) -> &mut Self {
        self.raw(name.as_bytes(), tar::EntryType::Regular, mode, b"", data);
        self
    }
    fn symlink(&mut self, name: &str, target: &str) -> &mut Self {
        self.raw(
            name.as_bytes(),
            tar::EntryType::Symlink,
            0o777,
            target.as_bytes(),
            b"",
        );
        self
    }
    fn dir(&mut self, name: &str) -> &mut Self {
        self.raw(name.as_bytes(), tar::EntryType::Directory, 0o755, b"", b"");
        self
    }
    fn gzip(&self, directory: &Path) -> PathBuf {
        let mut bytes = self.0.clone();
        bytes.extend(std::iter::repeat_n(0, 1024));
        let path = directory.join("archive.tar.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap();
        path
    }
}

fn unpack(
    builder: &TarBuilder,
    limits: &Limits,
) -> (tempfile::TempDir, Result<archive::Extracted, ArchiveError>) {
    let directory = tempfile::tempdir().unwrap();
    let archive = builder.gzip(directory.path());
    let tree = directory.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    let result = extract(
        &archive,
        ArchiveFormat::TarGz,
        &tree,
        limits,
        &AtomicBool::new(false),
    );
    (directory, result)
}

fn names(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink(), "extraction created a link");
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                found.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    found.sort();
    found
}

#[test]
fn release_layout_extracts_regular_files_and_keeps_one_soname_per_library_chain() {
    let mut builder = TarBuilder::default();
    builder
        .dir("llama-b1/")
        .file("llama-b1/llama-server", 0o755, b"server")
        .file("llama-b1/libllama.so.0.5.0", 0o755, b"library")
        .symlink("llama-b1/libllama.so.0", "libllama.so.0.5.0")
        .symlink("llama-b1/libllama.so", "libllama.so.0")
        .file("llama-b1/libggml-cpu.so", 0o755, b"backend")
        .symlink("llama-b1/dangling.so", "missing.so")
        .file("llama-b1/LICENSE", 0o644, b"MIT");
    let (directory, result) = unpack(&builder, &LIMITS);
    let extracted = result.unwrap();
    assert_eq!(extracted.files, 4);
    assert_eq!(extracted.aliases_materialized, 1);
    assert_eq!(extracted.aliases_dropped, 2);
    let tree = directory.path().join("tree");
    assert_eq!(
        names(&tree),
        [
            "llama-b1/LICENSE",
            "llama-b1/libggml-cpu.so",
            "llama-b1/libllama.so.0",
            "llama-b1/llama-server"
        ]
    );
    let root = archive::bundle_root(&tree).unwrap();
    assert_eq!(root, tree.join("llama-b1"));
    let described = describe(&root, Engine::LlamaCpp, &LIMITS, &AtomicBool::new(false)).unwrap();
    assert_eq!(described.bundle.entrypoint, PathBuf::from("llama-server"));
    assert_eq!(described.bundle.library_directories, [PathBuf::from(".")]);
    let roles: Vec<_> = described
        .bundle
        .files
        .iter()
        .map(|file| (file.path.to_string_lossy().into_owned(), file.role))
        .collect();
    use crate::runtime_bundle::FileRole;
    assert_eq!(
        roles,
        [
            ("LICENSE".into(), FileRole::Resource),
            ("libggml-cpu.so".into(), FileRole::Library),
            ("libllama.so.0".into(), FileRole::Library),
            ("llama-server".into(), FileRole::Executable),
        ]
    );
    assert_eq!(
        described.bundle.files[3].sha256,
        format!("{:x}", Sha256::digest(b"server"))
    );
    described
        .bundle
        .validate(crate::config::Provider::LlamaCpp)
        .unwrap();
    described
        .bundle
        .verify(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("llama-server"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        let mode = std::fs::metadata(root.join("LICENSE"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
    }
}

#[test]
fn traversal_absolute_links_escapes_devices_and_duplicates_are_refused_before_writing_outside() {
    type Case = (&'static str, Box<dyn Fn(&mut TarBuilder)>, &'static str);
    let cases: Vec<Case> = vec![
        (
            "parent",
            Box::new(|b| {
                b.file("../escape", 0o644, b"x");
            }),
            "archive_unsafe_path",
        ),
        (
            "nested-parent",
            Box::new(|b| {
                b.file("ok/../../escape", 0o644, b"x");
            }),
            "archive_unsafe_path",
        ),
        (
            "absolute",
            Box::new(|b| {
                b.file("/tmp/escape", 0o644, b"x");
            }),
            "archive_unsafe_path",
        ),
        (
            "backslash",
            Box::new(|b| {
                b.file("dir\\..\\escape", 0o644, b"x");
            }),
            "archive_unsafe_path",
        ),
        (
            "link-parent",
            Box::new(|b| {
                b.file("a", 0o644, b"x").symlink("l", "../../etc/passwd");
            }),
            "archive_link_escape",
        ),
        (
            "link-absolute",
            Box::new(|b| {
                b.symlink("l", "/etc/passwd");
            }),
            "archive_link_escape",
        ),
        (
            "link-then-file-below",
            Box::new(|b| {
                b.dir("real/")
                    .symlink("lib", "real")
                    .file("lib/x.so", 0o644, b"x");
            }),
            "archive_directory_link",
        ),
        (
            "duplicate",
            Box::new(|b| {
                b.file("a", 0o644, b"x").file("a", 0o644, b"y");
            }),
            "archive_duplicate_entry",
        ),
        (
            "file-over-dir",
            Box::new(|b| {
                b.dir("a/").file("a", 0o644, b"y");
            }),
            "archive_duplicate_entry",
        ),
        (
            "device",
            Box::new(|b| {
                b.raw(b"dev", tar::EntryType::Char, 0o644, b"", b"");
            }),
            "archive_unsupported_entry",
        ),
        (
            "fifo",
            Box::new(|b| {
                b.raw(b"pipe", tar::EntryType::Fifo, 0o644, b"", b"");
            }),
            "archive_unsupported_entry",
        ),
    ];
    for (name, build, code) in cases {
        let mut builder = TarBuilder::default();
        build(&mut builder);
        let (directory, result) = unpack(&builder, &LIMITS);
        assert_eq!(result.unwrap_err().code(), code, "{name}");
        assert!(!directory.path().join("escape").exists(), "{name}");
        assert!(!Path::new("/tmp/escape").exists() || name != "absolute");
        let leaked: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leaked
                .iter()
                .all(|entry| entry == "tree" || entry == "archive.tar.gz"),
            "{name}: {leaked:?}"
        );
    }
}

#[test]
fn entry_file_and_total_byte_limits_are_enforced() {
    let small = Limits {
        entries: 3,
        files: 2,
        file_bytes: 8,
        total_bytes: 12,
    };
    let mut many = TarBuilder::default();
    many.file("a", 0o644, b"1")
        .file("b", 0o644, b"2")
        .file("c", 0o644, b"3");
    assert_eq!(
        unpack(&many, &small).1.unwrap_err().code(),
        "archive_entry_limit"
    );
    let mut large = TarBuilder::default();
    large.file("a", 0o644, b"123456789");
    assert_eq!(
        unpack(&large, &small).1.unwrap_err().code(),
        "archive_size_limit"
    );
    let mut total = TarBuilder::default();
    total
        .file("a", 0o644, b"1234567")
        .file("b", 0o644, b"1234567");
    assert_eq!(
        unpack(&total, &small).1.unwrap_err().code(),
        "archive_size_limit"
    );
    let mut entries = TarBuilder::default();
    entries.dir("a/").dir("b/").dir("c/").dir("d/");
    assert_eq!(
        unpack(&entries, &small).1.unwrap_err().code(),
        "archive_entry_limit"
    );
    let cancelled = AtomicBool::new(true);
    let directory = tempfile::tempdir().unwrap();
    let path = many.gzip(directory.path());
    std::fs::create_dir(directory.path().join("tree")).unwrap();
    assert_eq!(
        extract(
            &path,
            ArchiveFormat::TarGz,
            &directory.path().join("tree"),
            &LIMITS,
            &cancelled
        )
        .unwrap_err()
        .code(),
        "cancelled"
    );
}

#[test]
fn xz_and_zip_archives_extract_through_the_same_path_rules() {
    let directory = tempfile::tempdir().unwrap();
    let mut builder = TarBuilder::default();
    builder
        .file("whisper-server", 0o755, b"speech")
        .file("libwhisper.so.1", 0o644, b"lib");
    let mut tar = builder.0.clone();
    tar.extend(std::iter::repeat_n(0, 1024));
    let xz_path = directory.path().join("archive.tar.xz");
    let mut writer = lzma_rust2::XzWriter::new(
        std::fs::File::create(&xz_path).unwrap(),
        lzma_rust2::XzOptions::default(),
    )
    .unwrap();
    writer.write_all(&tar).unwrap();
    writer.finish().unwrap();
    let tree = directory.path().join("xz");
    std::fs::create_dir(&tree).unwrap();
    extract(
        &xz_path,
        ArchiveFormat::TarXz,
        &tree,
        &LIMITS,
        &AtomicBool::new(false),
    )
    .unwrap();
    let described = describe(&tree, Engine::WhisperCpp, &LIMITS, &AtomicBool::new(false)).unwrap();
    assert_eq!(described.bundle.entrypoint, PathBuf::from("whisper-server"));
    described
        .bundle
        .validate(crate::config::Provider::WhisperCpp)
        .unwrap();

    let zip_bytes = |entries: &[(&str, u32, &[u8])]| {
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut cursor);
        for (name, mode, data) in entries {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .unix_permissions(*mode);
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        cursor.into_inner()
    };
    let good = directory.path().join("good.zip");
    std::fs::write(
        &good,
        zip_bytes(&[
            ("bin/llama-server", 0o755, b"server"),
            ("bin/librocblas.so.5", 0o644, b"lib"),
        ]),
    )
    .unwrap();
    let tree = directory.path().join("zip");
    std::fs::create_dir(&tree).unwrap();
    extract(
        &good,
        ArchiveFormat::Zip,
        &tree,
        &LIMITS,
        &AtomicBool::new(false),
    )
    .unwrap();
    let described = describe(&tree, Engine::LlamaCpp, &LIMITS, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        described.bundle.entrypoint,
        PathBuf::from("bin/llama-server")
    );
    assert_eq!(described.bundle.library_directories, [PathBuf::from("bin")]);
    let evil = directory.path().join("evil.zip");
    std::fs::write(&evil, zip_bytes(&[("../escape", 0o644, b"x")])).unwrap();
    let tree = directory.path().join("evil");
    std::fs::create_dir(&tree).unwrap();
    assert_eq!(
        extract(
            &evil,
            ArchiveFormat::Zip,
            &tree,
            &LIMITS,
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .code(),
        "archive_unsafe_path"
    );
    assert!(!directory.path().join("escape").exists());
}

#[test]
fn describe_requires_one_shallow_entrypoint() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("llama-cli"), b"x").unwrap();
    assert_eq!(
        describe(
            directory.path(),
            Engine::LlamaCpp,
            &LIMITS,
            &AtomicBool::new(false)
        )
        .err()
        .unwrap()
        .code(),
        "archive_entrypoint_missing"
    );
    for dir in ["a", "b"] {
        std::fs::create_dir(directory.path().join(dir)).unwrap();
        std::fs::write(directory.path().join(dir).join("llama-server"), b"x").unwrap();
    }
    assert_eq!(
        describe(
            directory.path(),
            Engine::LlamaCpp,
            &LIMITS,
            &AtomicBool::new(false)
        )
        .err()
        .unwrap()
        .code(),
        "archive_layout_unsupported"
    );
}

fn gguf(entries: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
    fn string(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend((value.len() as u64).to_le_bytes());
        bytes.extend(value);
    }
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend((entries.len() as u64).to_le_bytes());
    for (key, kind, value) in entries {
        string(&mut bytes, key.as_bytes());
        bytes.extend(kind.to_le_bytes());
        bytes.extend(value);
    }
    bytes
}

fn text(value: &str) -> Vec<u8> {
    let mut bytes = (value.len() as u64).to_le_bytes().to_vec();
    bytes.extend(value.as_bytes());
    bytes
}

fn qwen3_gguf() -> Vec<u8> {
    let u32_value = |value: u32| value.to_le_bytes().to_vec();
    gguf(&[
        ("qwen3.block_count", 4, u32_value(28)),
        ("general.architecture", 8, text("qwen3")),
        ("qwen3.context_length", 4, u32_value(40960)),
        ("qwen3.embedding_length", 4, u32_value(1024)),
        ("qwen3.attention.head_count", 4, u32_value(16)),
        ("qwen3.attention.head_count_kv", 4, u32_value(8)),
        ("qwen3.attention.key_length", 4, u32_value(128)),
        ("qwen3.attention.value_length", 4, u32_value(128)),
        (
            "qwen3.rope.freq_base",
            6,
            1_000_000f32.to_le_bytes().to_vec(),
        ),
        ("llama.block_count", 4, u32_value(99)),
    ])
}

#[test]
fn gguf_and_whisper_weights_are_detected_and_other_bytes_refused() {
    let directory = tempfile::tempdir().unwrap();
    let model = directory.path().join("Qwen3-0.6B-Q4_0.gguf");
    std::fs::write(&model, qwen3_gguf()).unwrap();
    let (format, bytes) = models::detect(&model).unwrap();
    assert_eq!(bytes, qwen3_gguf().len() as u64);
    assert_eq!(format.engine(), Engine::LlamaCpp);
    let ModelFormat::Gguf { declared, shape } = &format else {
        panic!("GGUF expected");
    };
    assert_eq!(declared.architecture.as_deref(), Some("qwen3"));
    assert_eq!(declared.training_context_tokens, Some(40960));
    assert_eq!(shape.block_count, Some(28));
    assert_eq!(shape.head_count_kv, Some(8));
    assert_eq!(shape.key_length, Some(128));

    let whisper = directory.path().join("ggml-tiny.bin");
    let mut header = 0x6767_6d6cu32.to_le_bytes().to_vec();
    for value in [51865i32, 1500, 384, 6, 4, 448, 384, 6, 4, 80, 1] {
        header.extend(value.to_le_bytes());
    }
    std::fs::write(&whisper, &header).unwrap();
    let (format, _) = models::detect(&whisper).unwrap();
    assert_eq!(
        format,
        ModelFormat::WhisperGgml {
            vocabulary: 51865,
            mel_bins: 80,
            text_layers: 4
        }
    );
    assert_eq!(format.engine(), Engine::WhisperCpp);

    let other = directory.path().join("model.safetensors");
    std::fs::write(&other, b"{\"__metadata__\":{}}").unwrap();
    assert_eq!(
        models::detect(&other).unwrap_err(),
        FormatError::Unsupported
    );
    let mut generic = 0x6767_6d6cu32.to_le_bytes().to_vec();
    generic.extend([0u8; 44]);
    std::fs::write(&other, generic).unwrap();
    assert_eq!(
        models::detect(&other).unwrap_err(),
        FormatError::Unsupported
    );
    std::fs::write(&other, b"GGUF\x09\x00\x00\x00").unwrap();
    assert!(matches!(
        models::detect(&other).unwrap_err(),
        FormatError::Invalid(_)
    ));
}

#[test]
fn profiles_and_estimates_follow_the_documented_formula() {
    let directory = tempfile::tempdir().unwrap();
    let model = directory.path().join("model.gguf");
    std::fs::write(&model, qwen3_gguf()).unwrap();
    let (format, _) = models::detect(&model).unwrap();
    let file_bytes = 400 * 1024 * 1024;
    let context = models::context_tokens(&format);
    assert_eq!(context, Some(8192));
    let estimate = models::estimate(&format, file_bytes, context);
    assert_eq!(estimate.kv_bytes, 28 * 8192 * 8 * (128 + 128) * 2);
    assert_eq!(estimate.basis, "gguf-shape");
    assert_eq!(
        estimate.resident_bytes,
        file_bytes + estimate.kv_bytes + models::WORKSPACE_ALLOWANCE_BYTES
    );
    let package = format!("package-{}", "a".repeat(64));
    let inputs = |gpu| models::ProfileInputs {
        id: "Qwen3-0.6B-Q4_0",
        model_file: &model,
        artifact: None,
        artifact_set: None,
        package: &package,
        backend: if gpu { "cuda" } else { "cpu" },
        gpu,
        cpu_threads: Some(8),
        format: &format,
        estimate: &estimate,
        companion_arguments: &[],
    };
    let gpu = models::registration(&inputs(true));
    assert_eq!(gpu["execution_profile"]["options"]["gpu_layers"], 29);
    assert_eq!(gpu["execution_profile"]["options"]["context_tokens"], 8192);
    assert_eq!(gpu["execution_profile"]["options"]["parallel_slots"], 1);
    assert!(gpu["execution_profile"]["options"].get("threads").is_none());
    assert_eq!(gpu["max_inflight"], 1);
    assert_eq!(gpu["policy"], "disabled");
    let cpu = models::registration(&inputs(false));
    assert_eq!(cpu["execution_profile"]["options"]["gpu_layers"], 0);
    assert_eq!(cpu["execution_profile"]["options"]["threads"], 8);
    for registration in [gpu, cpu] {
        let crate::config::Registration(spec) = serde_json::from_value(registration).unwrap();
        assert_eq!(
            spec.execution_profile.runtime_package(),
            Some(package.as_str())
        );
        assert_eq!(
            spec.execution_profile.adapter,
            crate::config::Provider::LlamaCpp
        );
        assert_eq!(spec.resident_bytes, estimate.resident_bytes);
    }

    let mut shapeless = format.clone();
    if let ModelFormat::Gguf { shape, declared } = &mut shapeless {
        *shape = Default::default();
        declared.training_context_tokens = None;
    }
    assert_eq!(
        models::context_tokens(&shapeless),
        Some(models::UNKNOWN_CONTEXT_TOKENS)
    );
    let fallback = models::estimate(&shapeless, file_bytes, Some(4096));
    assert_eq!(fallback.basis, "file-fraction");
    assert_eq!(fallback.kv_bytes, file_bytes / 4);

    let whisper = ModelFormat::WhisperGgml {
        vocabulary: 51865,
        mel_bins: 80,
        text_layers: 4,
    };
    let estimate = models::estimate(&whisper, 75_000_000, None);
    assert_eq!(estimate.kv_bytes, 0);
    let registration = models::registration(&models::ProfileInputs {
        id: "ggml-tiny",
        model_file: &model,
        artifact: None,
        artifact_set: None,
        package: &package,
        backend: "cpu",
        gpu: false,
        cpu_threads: Some(8),
        format: &whisper,
        estimate: &estimate,
        companion_arguments: &[],
    });
    let crate::config::Registration(spec) = serde_json::from_value(registration).unwrap();
    assert_eq!(spec.operations, [crate::config::Operation::Transcription]);
    assert_eq!(
        models::default_id("https/x/Qwen3-0.6B-Q4_0.gguf").as_deref(),
        Some("Qwen3-0.6B-Q4_0")
    );
    assert_eq!(
        models::default_id("ggml-tiny.bin").as_deref(),
        Some("ggml-tiny")
    );
    assert_eq!(models::default_id("a,b.gguf"), None);
}

#[test]
fn release_documents_must_publish_a_sha256_digest_and_size_for_the_asset() {
    let document = br#"{"assets":[{"name":"a.tar.gz","size":12,"digest":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},{"name":"b.tar.gz","size":3,"digest":null}]}"#;
    assert_eq!(
        download::digest_from_release(document, "a.tar.gz").unwrap(),
        (
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            12
        )
    );
    for asset in ["b.tar.gz", "missing.tar.gz"] {
        assert_eq!(
            download::digest_from_release(document, asset).unwrap_err(),
            download::DownloadError::DigestUnavailable
        );
    }
    assert!(download::digest_from_release(b"not json", "a.tar.gz").is_err());
}

async fn serve(bytes: &'static [u8]) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(bytes).await;
        }
    });
    format!("http://{address}/asset")
}

#[tokio::test]
async fn downloads_verify_the_pinned_digest_and_size_while_streaming() {
    const BODY: &[u8] = b"verified engine archive bytes";
    let url = serve(BODY).await;
    let directory = tempfile::tempdir().unwrap();
    let policy = crate::artifacts::SourcePolicy::AllowLoopbackHttp;
    let sha = format!("{:x}", Sha256::digest(BODY));
    let progress = AtomicU64::new(0);
    let good = directory.path().join("good");
    download::fetch(
        &url,
        policy,
        &sha,
        BODY.len() as u64,
        &good,
        &progress,
        || false,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&good).unwrap(), BODY);
    assert_eq!(
        progress.load(std::sync::atomic::Ordering::Acquire),
        BODY.len() as u64
    );
    let wrong = "0".repeat(64);
    assert_eq!(
        download::fetch(
            &url,
            policy,
            &wrong,
            BODY.len() as u64,
            &directory.path().join("hash"),
            &progress,
            || false
        )
        .await
        .unwrap_err(),
        download::DownloadError::HashMismatch
    );
    assert_eq!(
        download::fetch(
            &url,
            policy,
            &sha,
            BODY.len() as u64 - 1,
            &directory.path().join("size"),
            &progress,
            || false
        )
        .await
        .unwrap_err(),
        download::DownloadError::SizeMismatch
    );
    assert_eq!(
        download::fetch(
            &url,
            crate::artifacts::SourcePolicy::PublicHttps,
            &sha,
            BODY.len() as u64,
            &directory.path().join("policy"),
            &progress,
            || false
        )
        .await
        .unwrap_err(),
        download::DownloadError::SourcePolicy
    );
    assert_eq!(
        download::fetch(
            &url,
            crate::artifacts::SourcePolicy::Offline,
            &sha,
            BODY.len() as u64,
            &directory.path().join("offline"),
            &progress,
            || false
        )
        .await
        .unwrap_err(),
        download::DownloadError::Offline
    );
    assert!(!directory.path().join("policy").exists());
}

fn tensor_gguf(entries: &[(&str, u32, Vec<u8>)], tensors: &[&str]) -> Vec<u8> {
    let mut bytes = gguf(entries);
    bytes[8..16].copy_from_slice(&(tensors.len() as u64).to_le_bytes());
    for name in tensors {
        bytes.extend(text(name));
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(64u64.to_le_bytes());
        bytes.extend(64u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
    }
    bytes
}

#[test]
fn diffusion_gguf_safetensors_and_kokoro_onnx_select_media_engines() {
    let directory = tempfile::tempdir().unwrap();
    let full = directory.path().join("sd_turbo.gguf");
    std::fs::write(
        &full,
        tensor_gguf(
            &[],
            &[
                "cond_stage_model.transformer.text_model.final_layer_norm.bias",
                "first_stage_model.decoder.conv_in.weight",
                "model.diffusion_model.input_blocks.0.0.weight",
            ],
        ),
    )
    .unwrap();
    let (format, _) = models::detect(&full).unwrap();
    assert_eq!(format.engine(), Engine::StableDiffusionCpp);
    assert!(format.standalone());
    assert_eq!(
        format,
        ModelFormat::Diffusion {
            container: models::Container::Gguf,
            architecture: None,
            includes_vae: true,
            includes_text_encoder: true,
        }
    );

    let flux = directory.path().join("flux.gguf");
    std::fs::write(
        &flux,
        tensor_gguf(
            &[("general.architecture", 8, text("flux"))],
            &["double_blocks.0.img_attn.proj.weight"],
        ),
    )
    .unwrap();
    let (format, _) = models::detect(&flux).unwrap();
    assert_eq!(format.engine(), Engine::StableDiffusionCpp);
    assert!(!format.standalone());

    let llm = directory.path().join("llm.gguf");
    std::fs::write(
        &llm,
        tensor_gguf(&[], &["token_embd.weight", "blk.0.attn_q.weight"]),
    )
    .unwrap();
    assert_eq!(models::detect(&llm).unwrap().0.engine(), Engine::LlamaCpp);

    let safetensors = directory.path().join("sd.safetensors");
    let header = br#"{"__metadata__":{"format":"pt"},"model.diffusion_model.out.2.weight":{"dtype":"F16","shape":[4],"data_offsets":[0,8]},"first_stage_model.decoder.conv_out.bias":{"dtype":"F16","shape":[3],"data_offsets":[8,14]}}"#;
    let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(header);
    bytes.extend([0u8; 14]);
    std::fs::write(&safetensors, &bytes).unwrap();
    let (format, _) = models::detect(&safetensors).unwrap();
    assert_eq!(
        format,
        ModelFormat::Diffusion {
            container: models::Container::Safetensors,
            architecture: None,
            includes_vae: true,
            includes_text_encoder: false,
        }
    );
    let text_only =
        br#"{"model.embed_tokens.weight":{"dtype":"F16","shape":[2],"data_offsets":[0,4]}}"#;
    let mut bytes = (text_only.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(text_only);
    bytes.extend([0u8; 4]);
    std::fs::write(&safetensors, &bytes).unwrap();
    assert_eq!(
        models::detect(&safetensors).unwrap_err(),
        FormatError::Unsupported
    );

    let onnx = directory.path().join("kokoro-v1.0.onnx");
    std::fs::write(&onnx, b"\x08\x09\x12\x07pytorch\x1a\x052.6.0").unwrap();
    let (format, _) = models::detect(&onnx).unwrap();
    assert_eq!(format, ModelFormat::Onnx { ir_version: 9 });
    assert_eq!(format.engine(), Engine::Kokoros);

    let voices = directory.path().join("voices-v1.0.bin");
    {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&voices).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for name in ["am_adam.npy", "af_heart.npy"] {
            writer.start_file(name, options).unwrap();
            writer.write_all(b"\x93NUMPY").unwrap();
        }
        writer.finish().unwrap();
    }
    assert_eq!(models::voices(&voices).unwrap(), ["af_heart", "am_adam"]);
    {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&voices).unwrap());
        writer
            .start_file("../escape.npy", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
    }
    assert_eq!(
        models::voices(&voices).unwrap_err(),
        FormatError::Invalid("invalid_voice_pack")
    );
    assert_eq!(models::voices(&onnx).unwrap_err(), FormatError::Unsupported);
}

#[test]
fn media_registrations_bind_every_companion_and_pass_the_argument_grammar() {
    use crate::artifacts::ArtifactRole;
    let package = format!("package-{}", "b".repeat(64));
    let model = PathBuf::from("weights");
    let full = ModelFormat::Diffusion {
        container: models::Container::Gguf,
        architecture: None,
        includes_vae: true,
        includes_text_encoder: true,
    };
    let split = ModelFormat::Diffusion {
        container: models::Container::Gguf,
        architecture: Some("z_image".into()),
        includes_vae: false,
        includes_text_encoder: false,
    };
    let kokoro = ModelFormat::Onnx { ir_version: 9 };
    let companion = |name: &str, role| models::Companion {
        name: name.into(),
        role,
    };
    assert_eq!(
        models::companion_arguments(&split, &[]).unwrap_err(),
        models::CompanionError::Required("diffusion_companions_required")
    );
    assert_eq!(
        models::companion_arguments(&kokoro, &[]).unwrap_err(),
        models::CompanionError::Required("kokoro_voices_required")
    );
    assert_eq!(
        models::companion_arguments(&kokoro, &[companion("voices.bin", ArtifactRole::Vae)])
            .unwrap_err(),
        models::CompanionError::Unsupported
    );
    let split_companions = [
        companion("ae.safetensors", ArtifactRole::Vae),
        companion("qwen_3_4b.safetensors", ArtifactRole::Encoder),
    ];
    let split_arguments = models::companion_arguments(&split, &split_companions).unwrap();
    assert_eq!(
        split_arguments,
        [
            "--vae",
            "{member:ae.safetensors}",
            "--llm",
            "{member:qwen_3_4b.safetensors}"
        ]
    );
    let voice = vec![companion("voices-v1.0.bin", ArtifactRole::Voice)];
    let voice_arguments = models::companion_arguments(&kokoro, &voice).unwrap();
    for (format, backend, arguments, companions, operation) in [
        (
            &full,
            "cuda",
            Vec::new(),
            Vec::new(),
            crate::config::Operation::ImageGeneration,
        ),
        (
            &split,
            "vulkan",
            split_arguments.clone(),
            split_companions.to_vec(),
            crate::config::Operation::ImageGeneration,
        ),
        (
            &kokoro,
            "cpu",
            voice_arguments.clone(),
            voice.clone(),
            crate::config::Operation::Speech,
        ),
    ] {
        let estimate = models::estimate(format, 2_000_000_000, None);
        assert_eq!(estimate.basis, "weights-plus-workspace");
        let registration = models::registration(&models::ProfileInputs {
            id: "media",
            model_file: &model,
            artifact: None,
            artifact_set: None,
            package: &package,
            backend,
            gpu: backend != "cpu",
            cpu_threads: Some(8),
            format,
            estimate: &estimate,
            companion_arguments: &arguments,
        });
        let crate::config::Registration(spec) = serde_json::from_value(registration).unwrap();
        assert_eq!(spec.operations, [operation]);
        assert_eq!(spec.max_inflight, 1);
        let (template, _) = spec.execution_profile.resolved_arguments().unwrap();
        template
            .verify_members(
                companions
                    .iter()
                    .map(|member| (member.name.as_str(), member.role)),
            )
            .unwrap();
        let args = &spec.execution_profile.args;
        if matches!(format, ModelFormat::Diffusion { .. }) {
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--listen-ip", "127.0.0.1"])
            );
            assert_eq!(
                args.contains(&"--diffusion-model".to_owned()),
                !format.standalone()
            );
            assert_eq!(
                args.contains(&"--vae-tiling".to_owned()),
                backend == "vulkan"
            );
        } else {
            let server = args.iter().position(|arg| arg == "openai").unwrap();
            assert!(
                args[server..]
                    .windows(2)
                    .any(|pair| pair == ["--ip", "127.0.0.1"])
            );
            assert!(args[..server].contains(&"--data".to_owned()));
        }
    }
    assert_eq!(
        models::default_id("kokoro-v1.0.onnx").as_deref(),
        Some("kokoro-v1.0")
    );
    assert_eq!(
        models::default_id("sd_turbo.safetensors").as_deref(),
        Some("sd_turbo")
    );
}

#[test]
fn image_and_speech_engines_resolve_pinned_linux_builds() {
    let host = report(&rtx_host());
    let cuda = resolve(Engine::StableDiffusionCpp, "cuda", &host).unwrap();
    assert_eq!(cuda.repository, "lemonade-sdk/stable-diffusion.cpp");
    assert_eq!(cuda.tag, "master-957-90e86d1");
    assert_eq!(
        cuda.asset,
        "sd-master-90e86d1-ubuntu-cuda-sm_120-x64.tar.xz"
    );
    assert_eq!(
        cuda.sha256,
        Some("20b6e9136bf93b484c63340f3fca8f2256d5509f206d67ff67818df217b8612b")
    );
    let cpu = report(&cpu_only_host());
    assert_eq!(
        resolve(Engine::StableDiffusionCpp, "cpu", &cpu)
            .unwrap()
            .format,
        ArchiveFormat::Zip
    );
    assert_eq!(
        resolve(Engine::StableDiffusionCpp, "cuda", &cpu).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    let kokoros = resolve(Engine::Kokoros, "cpu", &cpu).unwrap();
    assert_eq!(kokoros.asset, "kokoros-linux-x86_64.tar.gz");
    assert_eq!(kokoros.repository, "lemonade-sdk/Kokoros");
    assert_eq!(
        resolve(Engine::Kokoros, "cuda", &host).unwrap_err(),
        Unresolved::BackendUnknown
    );
    for (name, engine) in [
        ("sd.cpp", Engine::StableDiffusionCpp),
        ("stable-diffusion-cpp", Engine::StableDiffusionCpp),
        ("kokoros", Engine::Kokoros),
        ("kokoro", Engine::Kokoros),
    ] {
        assert_eq!(Engine::parse(name), Some(engine));
        assert_eq!(
            Engine::parse(engine.provider().implementation_id().unwrap()),
            Some(engine)
        );
    }
    assert_eq!(
        inventory(&host)
            .recommendations
            .get("sd-cpp")
            .cloned()
            .flatten()
            .as_deref(),
        Some("cuda")
    );
}

/// Krackan Point pairs an XDNA 2 NPU with a gfx1152 iGPU, which no pinned ROCm llama.cpp or
/// sd.cpp release publishes.
fn krackan_host() -> HostReport {
    let tree = strix_host(true);
    tree.file(
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "cpu_cores_count 0\nsimd_count 16\ngfx_target_version 110502\nvendor_id 4098\ndevice_id 5390\ndrm_render_minor 128\n",
    );
    report(&tree)
}

#[test]
fn unpublished_recommendations_fall_to_the_best_published_backend() {
    let host = krackan_host();
    let inventory = inventory(&host);
    assert_eq!(
        inventory.recommendations["llamacpp"].as_deref(),
        Some("rocm")
    );
    assert_eq!(
        resolve(Engine::LlamaCpp, "rocm", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    let recommendation = recommended(Engine::LlamaCpp, &inventory, &host).unwrap();
    assert_eq!(recommendation.backend, "vulkan");
    assert_eq!(recommendation.unpublished.as_deref(), Some("rocm"));
    let images = recommended(Engine::StableDiffusionCpp, &inventory, &host).unwrap();
    assert_eq!(images.backend, "vulkan");
    let installed = [Installed {
        package: "vulkan-package".into(),
        engine: Some(Engine::LlamaCpp),
        backend: Some("vulkan".into()),
        gpu_target: None,
        tag: Some("b12032".into()),
        installed: true,
        declared_host_match: true,
    }];
    let choice = choose(Engine::LlamaCpp, &inventory, &host, &installed, None).unwrap();
    assert_eq!(choice.backend, "vulkan");
    assert!(choice.reason.contains("no rocm build"), "{}", choice.reason);
    let rows = installable(&inventory, &host, &installed);
    let row = |backend: &str| {
        rows.iter()
            .find(|row| row.engine == Engine::LlamaCpp && row.backend == backend)
            .unwrap()
    };
    assert!(row("vulkan").recommended);
    assert!(!row("rocm").recommended);
    assert_eq!(
        row("hybrid-rocm").unresolved,
        Some(Unresolved::NoMatchingGpu)
    );
    let strix = report(&strix_host(true));
    let strix_inventory = super::tests::inventory(&strix);
    let recommendation = recommended(Engine::LlamaCpp, &strix_inventory, &strix).unwrap();
    assert_eq!(recommendation.backend, "rocm");
    assert_eq!(recommendation.unpublished, None);
}

#[test]
fn unused_directory_aliases_are_dropped() {
    let mut builder = TarBuilder::default();
    builder
        .dir("d/")
        .file("d/f", 0o644, b"x")
        .symlink("l", "d")
        .symlink("m", "l");
    let (directory, result) = unpack(&builder, &LIMITS);
    assert_eq!(result.unwrap().aliases_dropped, 2);
    assert_eq!(names(&directory.path().join("tree")), ["d/f"]);
}

#[test]
fn library_aliases_in_other_directories_keep_one_copy_there() {
    let mut builder = TarBuilder::default();
    builder
        .dir("fastflowlm/")
        .file("fastflowlm/flm-real", 0o755, b"binary")
        .file("fastflowlm/lib/libxrt_core.so.2.25.00", 0o755, b"core")
        .symlink("fastflowlm/lib/libxrt_core.so.2", "libxrt_core.so.2.25.00")
        .symlink("fastflowlm/lib/libxrt_core.so", "libxrt_core.so.2")
        .symlink(
            "fastflowlm/lib/x86_64-linux-gnu/libxrt_core.so.2.25.00",
            "../libxrt_core.so.2.25.00",
        )
        .symlink(
            "fastflowlm/lib/x86_64-linux-gnu/libxrt_core.so.2",
            "../libxrt_core.so.2.25.00",
        )
        .symlink(
            "fastflowlm/lib/x86_64-linux-gnu/notes",
            "../libxrt_core.so.2.25.00",
        );
    let (directory, result) = unpack(&builder, &LIMITS);
    let extracted = result.unwrap();
    assert_eq!(extracted.files, 3);
    let tree = directory.path().join("tree");
    assert_eq!(
        names(&tree),
        [
            "fastflowlm/flm-real",
            "fastflowlm/lib/libxrt_core.so.2",
            "fastflowlm/lib/x86_64-linux-gnu/libxrt_core.so.2",
        ]
    );
    assert_eq!(
        std::fs::read(tree.join("fastflowlm/lib/x86_64-linux-gnu/libxrt_core.so.2")).unwrap(),
        b"core"
    );
    let root = archive::bundle_root(&tree).unwrap();
    let described = describe(&root, Engine::FastFlowLm, &LIMITS, &AtomicBool::new(false)).unwrap();
    assert_eq!(described.bundle.entrypoint, PathBuf::from("flm-real"));
    assert_eq!(described.bundle.implementation_id, "fastflowlm");
    assert_eq!(
        described.bundle.library_directories,
        [
            PathBuf::from("."),
            "lib".into(),
            "lib/x86_64-linux-gnu".into()
        ]
    );
}

/// Extracts and describes the published FLM and hybrid archives exactly as installation
/// does. Set JUICED_TEST_REAL_FLM_ARCHIVE and/or JUICED_TEST_REAL_HYBRID_ARCHIVE to the
/// downloaded release assets; the digests are checked against the pinned catalog rows.
#[test]
#[ignore = "needs downloaded release archives"]
fn published_npu_archives_extract_and_describe_within_bundle_limits() {
    let mut checked = 0;
    for (variable, backend) in [
        ("JUICED_TEST_REAL_FLM_ARCHIVE", "npu"),
        ("JUICED_TEST_REAL_HYBRID_ARCHIVE", "hybrid-rocm"),
    ] {
        let Some(path) = std::env::var_os(variable) else {
            continue;
        };
        let build = catalog::BUILDS
            .iter()
            .find(|build| build.backend == backend)
            .unwrap();
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            build.archive_bytes,
            "{variable}"
        );
        assert_eq!(
            archive::file_sha256(Path::new(&path), &cancelled).unwrap(),
            build.sha256.unwrap(),
            "{variable}"
        );
        let directory = tempfile::tempdir_in(Path::new(&path).parent().unwrap()).unwrap();
        let tree = directory.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let extracted =
            extract(Path::new(&path), build.format, &tree, &LIMITS, &cancelled).unwrap();
        let root = archive::bundle_root(&tree).unwrap();
        let described = describe(&root, build.engine, &LIMITS, &cancelled).unwrap();
        described.bundle.validate(build.engine.provider()).unwrap();
        let verified = described
            .bundle
            .verify(std::time::Instant::now() + std::time::Duration::from_secs(600))
            .unwrap();
        let mirrored = |relative: &str| root.join(relative).is_file();
        match backend {
            "npu" => {
                for library in [
                    "libxrt_core.so.2",
                    "libxrt_coreutil.so.2",
                    "libxrt_driver_xdna.so.2",
                ] {
                    assert!(mirrored(&format!("lib/{library}")), "{library}");
                    assert!(
                        mirrored(&format!("lib/x86_64-linux-gnu/{library}")),
                        "{library}"
                    );
                }
                assert!(mirrored("flm") && mirrored("xclbins/Gemma3-4B-NPU2/attn.xclbin"));
            }
            _ => {
                for library in [
                    "libxrt_core.so.2",
                    "libxrt_coreutil.so.2",
                    "libxrt_driver_xdna.so.2",
                ] {
                    assert!(
                        mirrored(&format!("xrt/lib/x86_64-linux-gnu/{library}")),
                        "{library}"
                    );
                }
                assert!(
                    mirrored("hybrid-server")
                        && mirrored("libggml-xdna.so")
                        && mirrored("bfp16_gemm.xclbin")
                );
            }
        }
        eprintln!(
            "{variable}: files={} bytes={} aliases_materialized={} aliases_dropped={} bundle_files={} library_directories={} entrypoint={} digest={}",
            extracted.files,
            extracted.bytes,
            extracted.aliases_materialized,
            extracted.aliases_dropped,
            described.bundle.files.len(),
            described.bundle.library_directories.len(),
            described.bundle.entrypoint.display(),
            verified.digest,
        );
        checked += 1;
    }
    assert!(checked > 0, "set a JUICED_TEST_REAL_*_ARCHIVE variable");
}
