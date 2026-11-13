use super::*;
use crate::hardware::tests::{report, strix_host};
use archive::{ArchiveError, assemble};
use catalog::{ArchiveFormat, THEROCK_GFX1150, Unresolved, resolve};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn inventory(host: &HostReport) -> Inventory {
    crate::hardware::project(&crate::catalog::Catalog::from_seed().unwrap(), host)
}

fn strix() -> HostReport {
    report(&strix_host(true))
}

fn installed(id: &str, engine: Engine, backend: &str, build: &Build) -> Installed {
    Installed {
        package: id.into(),
        engine: Some(engine),
        backend: Some(backend.into()),
        gpu_target: build.gpu_target.map(str::to_owned),
        tag: Some(build.tag.into()),
        installed: true,
        declared_host_match: true,
    }
}

#[test]
fn gfx1150_with_xdna2_recommends_rocm_chat_and_vulkan_media_like_native() {
    let host = strix();
    let inventory = inventory(&host);
    let recommended = |engine: Engine| {
        recommended(engine, &inventory, &host).map(|recommendation| recommendation.backend)
    };
    assert_eq!(recommended(Engine::LlamaCpp).as_deref(), Some("rocm"));
    assert_eq!(
        recommended(Engine::StableDiffusionCpp).as_deref(),
        Some("vulkan")
    );
    assert_eq!(recommended(Engine::WhisperCpp).as_deref(), Some("vulkan"));
    assert_eq!(recommended(Engine::Kokoros).as_deref(), Some("cpu"));
    assert_eq!(recommended(Engine::FastFlowLm).as_deref(), Some("npu"));
    for (engine, backend) in [
        (Engine::LlamaCpp, "rocm"),
        (Engine::LlamaCpp, "vulkan"),
        (Engine::LlamaCpp, "cpu"),
        (Engine::LlamaCpp, "hybrid-rocm"),
        (Engine::StableDiffusionCpp, "rocm"),
        (Engine::StableDiffusionCpp, "vulkan"),
        (Engine::StableDiffusionCpp, "cpu"),
        (Engine::WhisperCpp, "vulkan"),
        (Engine::WhisperCpp, "cpu"),
        (Engine::Kokoros, "cpu"),
    ] {
        let row = installable(&inventory, &host, &[])
            .into_iter()
            .find(|row| row.engine == engine && row.backend == backend)
            .unwrap();
        assert!(row.build.is_some(), "{engine:?} {backend}");
        assert_eq!(
            row.availability,
            Some(crate::hardware::Availability::Available),
            "{engine:?} {backend}"
        );
    }
}

#[test]
fn gfx1150_resolves_the_native_v0_9_2_pins() {
    let host = strix();
    let rocm = resolve(Engine::LlamaCpp, "rocm", &host).unwrap();
    assert_eq!(
        (rocm.repository, rocm.tag, rocm.gpu_target),
        ("lemonade-sdk/llama.cpp", "b11580", Some("gfx1150"))
    );
    assert_eq!(rocm.asset, "llama-b11580-bin-ubuntu-rocm-10.1-x64.tar.gz");
    assert_eq!(rocm.archive_bytes, 287482076);
    assert_eq!(
        rocm.sha256,
        Some("7655f6799c85f62801051b375e3ac4da98e34712f98279f8d300b1586a0325bd")
    );
    let runtime = rocm.runtime.unwrap();
    assert_eq!(runtime.asset, THEROCK_GFX1150.asset);
    assert_eq!(
        runtime.download_url(catalog::THEROCK_HOST),
        "https://stable.repo.amd.com/rocm/core/tarball/therock-dist-linux-gfx1150-10.1.0.tar.gz"
    );
    assert_eq!(runtime.archive_bytes, 1972096149);
    assert_eq!(
        runtime.sha256,
        "515df1170721668427a602807043719e59937109863e0baa7566875e9fccf070"
    );

    for backend in ["vulkan", "cpu"] {
        let build = resolve(Engine::LlamaCpp, backend, &host).unwrap();
        assert_eq!(
            (build.repository, build.tag),
            ("ggml-org/llama.cpp", "b12032")
        );
        assert!(build.runtime.is_none());
    }
    assert_eq!(
        resolve(Engine::LlamaCpp, "vulkan", &host).unwrap().sha256,
        Some("f75e7107f9450034c5467492c41e9ed91816e2d815fc4b802c6bd58de7d5b126")
    );

    let sd = resolve(Engine::StableDiffusionCpp, "rocm", &host).unwrap();
    assert_eq!(
        sd.asset,
        "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64-rocm-10.1.0.zip"
    );
    assert_eq!(sd.tag, "master-957-90e86d1");
    assert_eq!(sd.format, ArchiveFormat::Zip);
    assert_eq!(sd.runtime.unwrap().asset, THEROCK_GFX1150.asset);
    assert_eq!(
        resolve(Engine::StableDiffusionCpp, "vulkan", &host)
            .unwrap()
            .asset,
        "sd-master-90e86d1-bin-Linux-Ubuntu-24.04-x86_64-vulkan.zip"
    );
    assert_eq!(
        resolve(Engine::WhisperCpp, "vulkan", &host).unwrap().asset,
        "whisper-v1.8.4-linux-vulkan-x86_64.tar.gz"
    );
    assert_eq!(
        resolve(Engine::Kokoros, "cpu", &host).unwrap().tag,
        catalog::builds()
            .find(|build| build.engine == Engine::Kokoros)
            .unwrap()
            .tag
    );
    assert_eq!(resolve(Engine::Kokoros, "cpu", &host).unwrap().tag, "b21");
    assert_eq!(
        resolve(Engine::LlamaCpp, "hybrid-rocm", &host).unwrap().tag,
        "rocm-v0.3.2"
    );
}

#[test]
fn other_rocm_targets_keep_the_bundled_nightly_and_sd_rocm_stays_gfx1150_only() {
    let tree = strix_host(true);
    tree.file(
        "sys/class/kfd/kfd/topology/nodes/1/properties",
        "cpu_cores_count 0\nsimd_count 40\ngfx_target_version 110501\nvendor_id 4098\ndevice_id 5390\ndrm_render_minor 128\n",
    );
    let host = report(&tree);
    let rocm = resolve(Engine::LlamaCpp, "rocm", &host).unwrap();
    assert_eq!(rocm.asset, "llama-b1338-ubuntu-rocm-gfx1151-x64.zip");
    assert!(rocm.runtime.is_none());
    assert_eq!(
        resolve(Engine::StableDiffusionCpp, "rocm", &host).unwrap_err(),
        Unresolved::NoMatchingGpu
    );
    for build in catalog::builds().filter(|build| build.runtime.is_some()) {
        assert_eq!(build.gpu_target, Some("gfx1150"), "{}", build.asset);
    }
}

#[test]
fn adopted_backend_preferences_win_over_the_recommendation() {
    let host = strix();
    let inventory = inventory(&host);
    let build = |engine, backend| resolve(engine, backend, &host).unwrap();
    let packages = [
        installed(
            "package-llama-rocm",
            Engine::LlamaCpp,
            "rocm",
            &build(Engine::LlamaCpp, "rocm"),
        ),
        installed(
            "package-llama-vulkan",
            Engine::LlamaCpp,
            "vulkan",
            &build(Engine::LlamaCpp, "vulkan"),
        ),
    ];
    let automatic = choose(Engine::LlamaCpp, &inventory, &host, &packages, None).unwrap();
    assert_eq!(automatic.package, "package-llama-rocm");
    let vulkan = choose(
        Engine::LlamaCpp,
        &inventory,
        &host,
        &packages,
        Some("vulkan"),
    )
    .unwrap();
    assert_eq!(
        (vulkan.backend.as_str(), vulkan.package.as_str()),
        ("vulkan", "package-llama-vulkan")
    );
    assert!(vulkan.reason.starts_with("vulkan was requested llama.cpp"));
    let system = choose(
        Engine::LlamaCpp,
        &inventory,
        &host,
        &packages,
        Some("system"),
    )
    .unwrap();
    assert_eq!(system.package, "package-llama-vulkan");
    assert!(
        system
            .reason
            .starts_with("vulkan was requested as native system llama.cpp"),
        "{}",
        system.reason
    );
    assert_eq!(
        choose(
            Engine::StableDiffusionCpp,
            &inventory,
            &host,
            &[],
            Some("rocm")
        ),
        Err(ChoiceError::NotInstalled {
            backend: "rocm".into(),
            gpu_target: Some("gfx1150".into()),
            installable: true,
        })
    );
    assert_eq!(
        choose(Engine::StableDiffusionCpp, &inventory, &host, &[], None),
        Err(ChoiceError::NotInstalled {
            backend: "vulkan".into(),
            gpu_target: None,
            installable: true,
        })
    );
    assert_eq!(
        catalog::canonical_backend(Engine::StableDiffusionCpp, "system"),
        "system"
    );
}

#[test]
fn therock_members_keep_the_hip_blas_closure_and_drop_the_toolchain() {
    for kept in [
        ".info/version",
        ".kpack/blas_lib_gfx1150.kpack",
        ".kpack/solver_lib_gfx1150.kpack",
        "lib/libamdhip64.so.7",
        "lib/libamdhip64.so.7.16.26385-0000000",
        "lib/libhipblaslt.so.1.4",
        "lib/librocblas.so.5.7",
        "lib/rocblas/library/gfx1150/TensileLibrary.dat",
        "lib/hipblaslt/library/gfx1150/extop.co",
        "lib/llvm/lib/libLLVM.so.24.0git",
        "lib/llvm/lib/libclang-cpp.so.24.0git",
        "lib/llvm/lib/libomp.so",
        "lib/rocm_sysdeps/lib/librocm_sysdeps_numa.so.1",
        "share/doc/hip/LICENSE.md",
    ] {
        assert!(THEROCK_GFX1150.keeps(Path::new(kept)), "{kept}");
    }
    for dropped in [
        "bin/amd-smi",
        "include/hip/hip_runtime.h",
        "libexec/rocm-core/runner",
        "lib/libMIOpen.so.1.0",
        "lib/libdevice_conv_operations.a",
        "lib/llvm/bin/clang",
        "lib/llvm/lib/libomptarget.so.24.0git",
        "lib/llvm/lib/libgomp.so",
        "lib/rocm_sysdeps/lib/cmake/numa.cmake",
        "lib/rdc/grpc/lib/rocm_sysdeps/lib/librocm_sysdeps_z.so.1.3.2",
        ".kpack/miopen_lib_gfx1150.kpack",
    ] {
        assert!(!THEROCK_GFX1150.keeps(Path::new(dropped)), "{dropped}");
    }
}

#[derive(Default)]
struct Tar(Vec<u8>);

impl Tar {
    fn entry(&mut self, name: &str, kind: tar::EntryType, mode: u32, link: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        if !link.is_empty() {
            header.set_link_name(link).unwrap();
        }
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(data.len() as u64);
        header.set_cksum();
        self.0.extend_from_slice(header.as_bytes());
        self.0.extend_from_slice(data);
        self.0
            .extend(std::iter::repeat_n(0, (512 - data.len() % 512) % 512));
    }
    fn file(&mut self, name: &str, data: &[u8]) -> &mut Self {
        self.entry(name, tar::EntryType::Regular, 0o755, "", data);
        self
    }
    fn link(&mut self, name: &str, target: &str) -> &mut Self {
        self.entry(name, tar::EntryType::Symlink, 0o777, target, b"");
        self
    }
    fn write(&self, path: &Path) -> PathBuf {
        let mut bytes = self.0.clone();
        bytes.extend(std::iter::repeat_n(0, 1024));
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap();
        path.to_owned()
    }
}

fn rocm_build() -> Build {
    *catalog::builds()
        .find(|build| build.engine == Engine::LlamaCpp && build.runtime.is_some())
        .unwrap()
}

fn engine_archive(directory: &Path) -> PathBuf {
    let mut tar = Tar::default();
    tar.file("./llama-b11580/llama-server", b"server")
        .file("./llama-b11580/libggml-hip.so", b"hip backend")
        .file("./llama-b11580/libggml-base.so.0.26.0", b"base")
        .link("./llama-b11580/libggml-base.so.0", "libggml-base.so.0.26.0")
        .link("./llama-b11580/libggml-base.so", "libggml-base.so.0");
    tar.write(&directory.join("engine.tar.gz"))
}

fn runtime_archive(directory: &Path) -> PathBuf {
    let mut tar = Tar::default();
    tar.file("./.info/version", b"10.1.0\n")
        .file("./.kpack/blas_lib_gfx1150.kpack", b"blas kernels")
        .file("./.kpack/miopen_lib_gfx1150.kpack", b"unused")
        .file("./bin/amd-smi", b"tool")
        .file("./include/hip/hip_runtime.h", b"header")
        .file("./lib/libamdhip64.so.7.16.26385-0000000", b"hip")
        .link(
            "./lib/libamdhip64.so.7",
            "libamdhip64.so.7.16.26385-0000000",
        )
        .link("./lib/libamdhip64.so", "libamdhip64.so.7")
        .file("./lib/librocblas.so.5.7", b"rocblas")
        .link("./lib/librocblas.so.5", "librocblas.so.5.7")
        .file("./lib/libMIOpen.so.1.0", b"unused")
        .file("./lib/rocblas/library/TensileLibrary.dat", b"tensile")
        .file("./lib/llvm/lib/libLLVM.so.24.0git", b"llvm")
        .link("./lib/llvm/lib/libLLVM.so", "libLLVM.so.24.0git")
        .file("./lib/llvm/lib/libomp.so", b"omp")
        .link("./lib/llvm/lib/libgomp.so.1", "libomp.so")
        .file("./lib/rocm_sysdeps/lib/librocm_sysdeps_numa.so.1", b"numa")
        .link(
            "./lib/rocm_sysdeps/lib/libnuma.so",
            "librocm_sysdeps_numa.so.1",
        )
        .file("./share/doc/hip/LICENSE.md", b"MIT");
    tar.write(&directory.join("runtime.tar.gz"))
}

#[test]
fn rocm_bundles_carry_the_selected_runtime_members_under_their_own_root() {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine_archive(directory.path());
    let runtime = runtime_archive(directory.path());
    let tree = directory.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    let described = assemble(
        &rocm_build(),
        &engine,
        Some(&runtime),
        &tree,
        &AtomicBool::new(false),
    )
    .unwrap();
    let bundle = &described.bundle;
    assert_eq!(bundle.root, tree.join("llama-b11580"));
    assert_eq!(bundle.entrypoint, PathBuf::from("llama-server"));
    assert_eq!(
        bundle.library_directories,
        [
            PathBuf::from("."),
            PathBuf::from("rocm/lib"),
            PathBuf::from("rocm/lib/llvm/lib"),
            PathBuf::from("rocm/lib/rocm_sysdeps/lib"),
        ]
    );
    let files: Vec<_> = bundle
        .files
        .iter()
        .map(|file| file.path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        files,
        [
            "libggml-base.so.0",
            "libggml-hip.so",
            "llama-server",
            "rocm/.info/version",
            "rocm/.kpack/blas_lib_gfx1150.kpack",
            "rocm/lib/libamdhip64.so.7",
            "rocm/lib/librocblas.so.5",
            "rocm/lib/llvm/lib/libLLVM.so.24.0git",
            "rocm/lib/rocblas/library/TensileLibrary.dat",
            "rocm/lib/rocm_sysdeps/lib/librocm_sysdeps_numa.so.1",
            "rocm/share/doc/hip/LICENSE.md",
        ]
    );
    bundle.validate(crate::config::Provider::LlamaCpp).unwrap();
    bundle
        .verify(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn a_runtime_build_without_its_runtime_or_with_a_colliding_directory_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine_archive(directory.path());
    let runtime = runtime_archive(directory.path());
    let fresh = |name: &str| {
        let tree = directory.path().join(name);
        std::fs::create_dir(&tree).unwrap();
        tree
    };
    let stop = AtomicBool::new(false);
    assert!(matches!(
        assemble(&rocm_build(), &engine, None, &fresh("missing"), &stop),
        Err(ArchiveError::Layout)
    ));
    let vulkan = *catalog::builds()
        .find(|build| build.engine == Engine::LlamaCpp && build.backend == "vulkan")
        .unwrap();
    assert!(matches!(
        assemble(
            &vulkan,
            &engine,
            Some(&runtime),
            &fresh("unexpected"),
            &stop
        ),
        Err(ArchiveError::Layout)
    ));
    let mut colliding = Tar::default();
    colliding
        .file("./llama-b11580/llama-server", b"server")
        .file("./llama-b11580/rocm/lib/libamdhip64.so.7", b"own");
    let colliding = colliding.write(&directory.path().join("colliding.tar.gz"));
    assert!(matches!(
        assemble(
            &rocm_build(),
            &colliding,
            Some(&runtime),
            &fresh("collision"),
            &stop
        ),
        Err(ArchiveError::Layout)
    ));
}

#[test]
fn rocm_runtime_provenance_is_validated_with_the_release() {
    let runtime = &THEROCK_GFX1150;
    let mut release = crate::runtime_packages::ReleaseSource {
        engine: "llama.cpp".into(),
        backend: "rocm".into(),
        gpu_target: Some("gfx1150".into()),
        repository: "lemonade-sdk/llama.cpp".into(),
        tag: "b11580".into(),
        asset: "llama-b11580-bin-ubuntu-rocm-10.1-x64.tar.gz".into(),
        archive_sha256: "7655f6799c85f62801051b375e3ac4da98e34712f98279f8d300b1586a0325bd".into(),
        archive_bytes: 287482076,
        digest_source: "pinned".into(),
        runtime: Some(crate::runtime_packages::RuntimeSource {
            name: runtime.name.into(),
            version: runtime.version.into(),
            gpu_target: runtime.gpu_target.into(),
            url: runtime.download_url(catalog::THEROCK_HOST),
            archive_sha256: runtime.sha256.into(),
            archive_bytes: runtime.archive_bytes,
        }),
    };
    assert!(release.valid());
    let encoded = serde_json::to_value(&release).unwrap();
    assert_eq!(encoded["runtime"]["version"], "10.1.0");
    release.runtime.as_mut().unwrap().archive_sha256 = "X".repeat(64);
    assert!(!release.valid());
    release.runtime = None;
    assert!(
        serde_json::to_value(&release)
            .unwrap()
            .get("runtime")
            .is_none()
    );
}

/// Set JUICED_TEST_ROCM_ARCHIVES to a directory holding the pinned gfx1150 ROCm archives
/// under their release asset names. Nothing in them is executed.
#[test]
#[ignore = "needs the downloaded gfx1150 ROCm archives (about 2.5 GB)"]
fn real_gfx1150_rocm_archives_match_their_pins_and_assemble_into_bundles() {
    let directory = PathBuf::from(std::env::var_os("JUICED_TEST_ROCM_ARCHIVES").unwrap());
    let host = strix();
    let stop = AtomicBool::new(false);
    let check = |path: &Path, bytes: u64, sha256: &str| {
        assert_eq!(std::fs::metadata(path).unwrap().len(), bytes, "{path:?}");
        assert_eq!(
            archive::file_sha256(path, &stop).unwrap(),
            sha256,
            "{path:?}"
        );
    };
    let runtime_path = directory.join(THEROCK_GFX1150.asset);
    check(
        &runtime_path,
        THEROCK_GFX1150.archive_bytes,
        THEROCK_GFX1150.sha256,
    );
    for (engine, entrypoint) in [
        (Engine::LlamaCpp, "llama-server"),
        (Engine::StableDiffusionCpp, "bin/sd-server"),
    ] {
        let build = resolve(engine, "rocm", &host).unwrap();
        let archive_path = directory.join(build.asset);
        check(&archive_path, build.archive_bytes, build.sha256.unwrap());
        let scratch = tempfile::tempdir_in(&directory).unwrap();
        let tree = scratch.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let described = assemble(&build, &archive_path, Some(&runtime_path), &tree, &stop).unwrap();
        let bundle = &described.bundle;
        assert_eq!(bundle.entrypoint, PathBuf::from(entrypoint));
        bundle.validate(engine.provider()).unwrap();
        bundle
            .verify(std::time::Instant::now() + std::time::Duration::from_secs(600))
            .unwrap();
        let has = |path: &str| bundle.files.iter().any(|file| file.path == Path::new(path));
        for path in [
            "rocm/lib/libamdhip64.so.7",
            "rocm/lib/libhipblas.so.3",
            "rocm/lib/librocblas.so.5",
            "rocm/lib/libhsa-runtime64.so.1",
            "rocm/lib/libamd_comgr.so.3",
            "rocm/lib/llvm/lib/libLLVM.so.24.0git",
            "rocm/.kpack/blas_lib_gfx1150.kpack",
        ] {
            assert!(has(path), "{engine:?} lacks {path}");
        }
        let bytes: u64 = described.sizes.iter().map(|(_, bytes)| bytes).sum();
        eprintln!(
            "{} {}: {} files, {bytes} bytes, library directories {:?}, root {}",
            engine.name(),
            build.asset,
            bundle.files.len(),
            bundle.library_directories,
            bundle.root.display()
        );
        if std::env::var_os("JUICED_TEST_ROCM_KEEP").is_some() {
            let _ = scratch.keep();
        }
    }
}
