use juiced_service::{
    http::{Auth, router},
    supervisor::Manager,
};
use std::{
    future::IntoFuture, net::SocketAddr, path::PathBuf, process::ExitCode, sync::atomic::Ordering,
    time::Duration,
};

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let threads = std::env::var("JUICED_IO_THREADS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(2, |count| count.get().min(4))
        });
    if !(1..=64).contains(&threads) {
        return Err("JUICED_IO_THREADS must be between 1 and 64".into());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

async fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    let mut ui = None;
    let mut broadcast = false;
    let mut inspect_root = None;
    let mut inspect_timeout = None;
    let mut server_options = false;
    let mut serving_options = false;
    let mut native_source = None;
    let mut native_bindings = None;
    let mut native_aliases = false;
    let mut settings_action = None;
    let mut runtime_import = None;
    let mut listen: SocketAddr = "127.0.0.1:13306".parse()?;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--import-runtime-bundle" => {
                if runtime_import.is_some() {
                    return Err("Choose only one runtime bundle import".into());
                }
                runtime_import = Some(PathBuf::from(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--import-runtime-bundle needs a JSON specification")?,
                ));
            }
            "--backup-settings-copy" | "--restore-settings-copy" => {
                if settings_action.is_some() {
                    return Err("Choose only one offline settings backup or restore action".into());
                }
                let path =
                    PathBuf::from(args.next().filter(|value| !value.starts_with("--")).ok_or(
                        "Offline settings backup and restore need an explicit capsule directory",
                    )?);
                settings_action = Some((arg == "--restore-settings-copy", path));
            }
            "--config" => {
                server_options = true;
                config_path = Some(PathBuf::from(args.next().ok_or("--config needs a path")?))
            }
            "--listen" => {
                serving_options = true;
                server_options = true;
                listen = args.next().ok_or("--listen needs an address")?.parse()?;
            }
            "--ui" => {
                serving_options = true;
                server_options = true;
                ui = Some(PathBuf::from(args.next().ok_or("--ui needs a directory")?));
            }
            "--broadcast" => {
                serving_options = true;
                server_options = true;
                broadcast = true;
            }
            "--no-broadcast" => {
                serving_options = true;
                server_options = true;
                broadcast = false;
            }
            "--import-native-settings-copy" => {
                if native_source.is_some() {
                    return Err("Choose only one copied native source".into());
                }
                native_source = Some(PathBuf::from(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--import-native-settings-copy needs a copied source directory")?,
                ));
            }
            "--native-bindings" => {
                if native_bindings.is_some() {
                    return Err("Choose only one native runtime bindings file".into());
                }
                native_bindings = Some(PathBuf::from(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--native-bindings needs a JSON file")?,
                ));
            }
            "--include-native-aliases" => {
                if native_aliases {
                    return Err("Choose native aliases only once".into());
                }
                native_aliases = true;
            }
            "--inspect-snapshot-store" => {
                if inspect_root.is_some() {
                    return Err("Choose only one snapshot store to inspect".into());
                }
                inspect_root = Some(PathBuf::from(
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or("--inspect-snapshot-store needs a Rust cache directory")?,
                ));
            }
            "--inspect-timeout-ms" => {
                let millis: u64 = args
                    .next()
                    .ok_or("--inspect-timeout-ms needs milliseconds")?
                    .parse()?;
                if !(1..=300_000).contains(&millis) || inspect_timeout.is_some() {
                    return Err(
                        "Choose one inspection timeout between 1 and 300000 milliseconds".into(),
                    );
                }
                inspect_timeout = Some(Duration::from_millis(millis));
            }
            "--help" | "-h" => {
                println!(
                    "Juiced Rust experiment\nlemond --config PATH [--listen 127.0.0.1:13306] [--ui UI_DIST] [--broadcast]\nlemond --inspect-snapshot-store RUST_CACHE_DIR [--inspect-timeout-ms 5000]\nlemond --config PATH --import-native-settings-copy COPIED_SOURCE [--native-bindings JSON_FILE] [--include-native-aliases]\nlemond --config PATH --import-runtime-bundle IMPORT_SPEC_JSON\nlemond --config PATH --backup-settings-copy NEW_CAPSULE_DIRECTORY\nlemond --config NEW_DIRECTORY/config.json --restore-settings-copy CAPSULE_DIRECTORY\nRuntime import copies an exact local llama.cpp or whisper.cpp closure into private versioned storage while the service is stopped. It retains incomplete copies, never enables a model, and returns a BundleSpec for explicit selection. Declared provenance and target fields do not qualify hardware, ABI, licenses, or engine compatibility.\nSettings capsules preserve exact configuration and retained imported metadata, including pending ownership. They exclude weights, runtime packages and computation cache. Restore publishes configuration last and retries only matching owned files. Conflicting or partial file contents remain untouched.\nNative metadata import is offline and preserves its source. Explicit runtime bindings start Disabled. It does not replay native jobs/helpers or qualify a complete backup.\nInspection is read-only, non-atomic and grants no recovery authority. It does not migrate native snapshot formats.\nUses a separate rewrite config. Never point this at production configuration."
                );
                return Ok(ExitCode::SUCCESS);
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    if let Some(root) = inspect_root {
        if server_options
            || native_source.is_some()
            || native_bindings.is_some()
            || native_aliases
            || settings_action.is_some()
            || runtime_import.is_some()
        {
            return Err(
                "Snapshot inspection cannot be combined with server or native import options"
                    .into(),
            );
        }
        return inspect(root, inspect_timeout).await;
    }
    if inspect_timeout.is_some() {
        return Err("--inspect-timeout-ms requires --inspect-snapshot-store".into());
    }
    if let Some((restoring, capsule)) = settings_action {
        if serving_options
            || native_source.is_some()
            || native_bindings.is_some()
            || native_aliases
            || runtime_import.is_some()
        {
            return Err("Offline settings backup and restore cannot be combined with serving, native import or runtime import options".into());
        }
        let configuration = config_path
            .ok_or("--config PATH is required for offline settings backup or restore")?;
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let token = cancelled.clone();
        let mut work = tokio::task::spawn_blocking(move || {
            if restoring {
                juiced_service::settings_backup::restore(capsule, configuration, token)
            } else {
                juiced_service::settings_backup::export(configuration, capsule, token)
            }
        });
        let receipt = tokio::select! {
            result = &mut work => result??,
            _ = wait_for_shutdown() => {
                cancelled.store(true, Ordering::Release);
                work.await??
            }
        };
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(input) = runtime_import {
        if serving_options || native_source.is_some() || native_bindings.is_some() || native_aliases
        {
            return Err("Offline runtime import cannot be combined with serving, settings or native import options".into());
        }
        let configuration =
            config_path.ok_or("--config PATH is required for offline runtime import")?;
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let token = cancelled.clone();
        let mut work = tokio::task::spawn_blocking(move || {
            juiced_service::runtime_packages::import_file(configuration, input, token)
        });
        let receipt = tokio::select! {
            result=&mut work=>result??,
            _=wait_for_shutdown()=> {cancelled.store(true,Ordering::Release);work.await??}
        };
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(source) = native_source {
        if !cfg!(unix) {
            return Err("Native metadata import needs a qualified private-permission implementation on this platform".into());
        }
        if serving_options {
            return Err("Offline native import cannot be combined with serving options".into());
        }
        let config_path =
            config_path.ok_or("--config PATH is required for offline native import")?;
        let bindings = match native_bindings {
            Some(path) => read_native_bindings(path)?,
            None => Vec::new(),
        };
        let source = juiced_service::native_store::SourceCohort::inspect(
            source,
            native_aliases,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )?;
        juiced_service::native_execution::ResolutionContext::from_source(&source)?;
        let manager = Manager::open_for_native_import(config_path)?;
        let result = manager.import_native_copy(source, bindings).await;
        let stopped = manager.shutdown().await;
        if !stopped.is_settled() {
            return Err("Offline import shutdown has unsettled owned work; preserve the private metadata store".into());
        }
        println!("{}", serde_json::to_string(&result?)?);
        return Ok(ExitCode::SUCCESS);
    }
    if native_bindings.is_some() || native_aliases {
        return Err("Native bindings and aliases require --import-native-settings-copy".into());
    }
    #[cfg(all(unix, feature = "cpu-profile"))]
    let profile = juiced_service::profiling::CpuProfile::from_env()?;
    let config_path = config_path.ok_or("--config PATH is required")?;
    let manager = Manager::open(config_path)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!(
        "Juiced Rust experiment listening on http://{}",
        listener.local_addr()?
    );
    let bound = listener.local_addr()?;
    juiced_service::wyoming::set_http_port(bound.port());
    manager.logs().service(
        juiced_service::logs::Level::Info,
        &format!("Service listening on http://{bound}"),
    );
    let beacon = if broadcast
        && bound.is_ipv4()
        && (bound.ip().is_loopback() || bound.ip().is_unspecified())
    {
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "Juiced rewrite".to_owned());
        Some(tokio::spawn(async move {
            if let Err(error) = juiced_service::discovery::broadcast_local(
                bound.port(),
                hostname,
                Duration::from_secs(2),
            )
            .await
            {
                eprintln!("Local discovery stopped: {error}");
            }
        }))
    } else {
        None
    };
    manager.start_residency();
    manager.recover_acquisitions().await;
    let adoption = juiced_service::supervisor::native_adoption_from_env()
        .map(|(source, roots)| manager.start_native_adoption(source, roots));
    let startup_manager = manager.clone();
    let startup = tokio::spawn(async move { startup_manager.start_hot().await });
    let shutdown_manager = manager.clone();
    let startup_abort = startup.abort_handle();
    let (shutdown_start, mut shutdown_started) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, router(manager.clone(), Auth::from_env(), ui))
        .with_graceful_shutdown(async move {
            wait_for_shutdown().await;
            startup_abort.abort();
            let _ = shutdown_start.send(tokio::time::Instant::now() + Duration::from_secs(5));
            let _ = shutdown_manager.shutdown().await;
        });
    let mut server = std::pin::pin!(server.into_future());
    let (served, deadline) = tokio::select! {
        result = &mut server => (Some(result), shutdown_started.try_recv().ok()),
        started = &mut shutdown_started => match started {
            Ok(deadline) => (tokio::time::timeout_at(deadline, &mut server).await.ok(), Some(deadline)),
            Err(_) => (Some(server.await), None),
        }
    };
    let graceful_completed = served.is_some();
    let serve_error = served.and_then(Result::err);
    startup.abort();
    if let Some(adoption) = adoption {
        adoption.abort();
    }
    let deadline = deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(5));
    let _ = tokio::time::timeout_at(deadline, startup).await;
    if let Some(beacon) = beacon {
        beacon.abort();
        let _ = beacon.await;
    }
    let settled = tokio::time::timeout_at(deadline, manager.shutdown())
        .await
        .is_ok_and(|report| report.is_settled());
    #[cfg(all(unix, feature = "cpu-profile"))]
    if let Some(profile) = profile {
        profile.finish()?;
    }
    if let Some(error) = serve_error {
        return Err(error.into());
    }
    if !graceful_completed || !settled {
        eprintln!(
            "Shutdown ended without confirming all worker, artifact-writer and power-guard cleanup. Resources were retained while the manager remained alive."
        );
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

fn read_native_bindings(
    path: PathBuf,
) -> Result<Vec<juiced_service::config::ModelSpec>, Box<dyn std::error::Error>> {
    let path = std::path::absolute(path)?;
    let reader = juiced_service::storage::DirectoryReader::open_existing(
        path.parent()
            .ok_or("Native bindings need a parent directory")?,
    )?;
    let observed = reader.observe(
        path.file_name().ok_or("Native bindings need a file name")?,
        2 * 1024 * 1024,
    )?;
    let bytes = observed.bytes().ok_or("Native bindings file is missing")?;
    let models: Vec<juiced_service::config::ModelSpec> = serde_json::from_slice(bytes)
        .map_err(|_| "Native bindings must be a supported JSON array no larger than 2 MiB")?;
    for model in &models {
        if model.execution_profile.native_model_id().is_none()
            || model.policy != juiced_service::config::Policy::Disabled
            || model.revision != 0
        {
            return Err(
                "Native bindings require native-owned profiles, Disabled policy and revision zero"
                    .into(),
            );
        }
    }
    observed.verify()?;
    Ok(models)
}

async fn inspect(
    root: PathBuf,
    timeout: Option<Duration>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut options = juiced_service::cache::SnapshotInspectionOptions::default();
    if let Some(timeout) = timeout {
        options.deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or("Inspection timeout is unavailable")?;
    }
    let cancelled = options.cancelled.clone();
    let mut inspection = tokio::task::spawn_blocking(move || {
        juiced_service::cache::inspect_snapshot_store(root, &options)
    });
    let report = tokio::select! {
        result = &mut inspection => result?,
        _ = wait_for_shutdown() => { cancelled.store(true, Ordering::Release); inspection.await? }
    };
    serde_json::to_writer(std::io::stdout().lock(), &report)?;
    println!();
    Ok(if report.validation_complete && report.issues.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
