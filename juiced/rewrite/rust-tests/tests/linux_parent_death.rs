#![cfg(target_os = "linux")]

mod support;

use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use support::Server;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Subreaper {
    previous: libc::c_int,
}
impl Subreaper {
    fn enter() -> Self {
        let mut previous = 0;
        unsafe {
            assert_eq!(
                libc::prctl(
                    libc::PR_GET_CHILD_SUBREAPER,
                    &mut previous as *mut libc::c_int,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong
                ),
                0
            );
            assert_eq!(
                libc::prctl(
                    libc::PR_SET_CHILD_SUBREAPER,
                    1 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong
                ),
                0
            );
        }
        Self { previous }
    }
}
impl Drop for Subreaper {
    fn drop(&mut self) {
        unsafe {
            libc::prctl(
                libc::PR_SET_CHILD_SUBREAPER,
                self.previous as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            );
        }
    }
}
struct OwnedWorker {
    pid: libc::pid_t,
    reaped: bool,
}
impl OwnedWorker {
    fn reap(&mut self) -> i32 {
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(self.pid, &mut status, 0) }, self.pid);
        self.reaped = true;
        status
    }
}
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let mut status = 0;
        match unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) } {
            0 => unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                libc::waitpid(self.pid, &mut status, 0);
            },
            pid if pid == self.pid => {}
            _ => {}
        }
    }
}
fn process(pid: u32) -> Option<(char, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(") ")?;
    let mut fields = tail.split_whitespace();
    Some((fields.next()?.chars().next()?, fields.next()?.parse().ok()?))
}
async fn load(server: &Server, model: &str, stage: &str) -> Value {
    let started = std::time::Instant::now();
    let (status, response) = server.post("load", json!({"model":model})).await;
    let elapsed = started.elapsed();
    assert!(
        status.is_success(),
        "{stage}: status={status}; response={response}; elapsed={elapsed:?}; model={}; admission={}",
        server.model(model).await,
        server.get("juiced/admission").await
    );
    response
}
async fn prove_parent_death(real: Option<(PathBuf, PathBuf)>) {
    let _serial = SERIAL.lock().await;
    let _subreaper = Subreaper::enter();
    let unrelated = Server::new(true).await;
    load(
        &unrelated,
        "fixture.one",
        "load unrelated daemon before parent death",
    )
    .await;
    let unrelated_pid = unrelated.model("fixture.one").await["pid"]
        .as_u64()
        .unwrap() as u32;
    let mut owned = Server::configured(true, |config| {
        if let Some((engine, weights)) = real {
            config["resident_budget_bytes"] = json!(1024u64 * 1024 * 1024);
            config["startup_timeout_ms"] = json!(30000);
            config["request_timeout_ms"] = json!(30000);
            let model = &mut config["models"][0];
            model["command"] = json!(engine);
            model["model_file"] = json!(weights);
            model["provider"] = json!("llama-cpp");
            model["resident_bytes"] = json!(512u64 * 1024 * 1024);
            model["max_inflight"] = json!(1);
            model["args"] = json!([
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--model",
                "{model}",
                "--ctx-size",
                "256",
                "--parallel",
                "1",
                "--slots",
                "--n-gpu-layers",
                "0"
            ]);
        }
    })
    .await;
    load(
        &owned,
        "fixture.one",
        "load owned daemon before parent death",
    )
    .await;
    let worker_pid = owned.model("fixture.one").await["pid"].as_u64().unwrap() as u32;
    let mut worker = OwnedWorker {
        pid: worker_pid as libc::pid_t,
        reaped: false,
    };
    let before = process(worker_pid).expect("owned worker exists");
    assert_ne!(before.0, 'Z');
    assert_eq!(before.1, owned.pid());
    owned.kill_manager_only();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if process(worker_pid) == Some(('Z', std::process::id())) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned direct worker did not die and reparent after manager SIGKILL");
    let status = worker.reap();
    assert!(libc::WIFSIGNALED(status));
    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    assert!(process(worker_pid).is_none());
    owned.retire_reaped_manager_without_group_signal();
    let foreign = process(unrelated_pid).expect("unrelated worker remains alive");
    assert_ne!(foreign.0, 'Z');
    assert_eq!(foreign.1, unrelated.pid());
    let (status, result) = unrelated
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Unrelated worker pipeline"}),
        )
        .await;
    assert!(status.is_success());
    assert_eq!(result["model"], "fixture.one");
    assert_eq!(unrelated.model("fixture.one").await["pid"], unrelated_pid);
    owned.start().await;
    assert_eq!(owned.model("fixture.one").await["loaded"], false);
    assert_eq!(
        owned.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    load(
        &owned,
        "fixture.one",
        "reload owned daemon after parent death and restart",
    )
    .await;
    assert_ne!(owned.model("fixture.one").await["pid"], worker_pid);
}

#[tokio::test]
async fn killing_only_a_manager_terminates_its_direct_fixture_worker_and_preserves_other_services()
{
    prove_parent_death(None).await;
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_GGUF; Linux direct-process parent-death proof"]
async fn killing_only_a_manager_terminates_its_direct_real_llama_worker() {
    let engine = PathBuf::from(
        std::env::var_os("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
    );
    let weights =
        PathBuf::from(std::env::var_os("JUICED_TEST_GGUF").expect("set JUICED_TEST_GGUF"));
    assert!(engine.is_file());
    assert!(weights.is_file());
    prove_parent_death(Some((engine, weights))).await;
}

#[tokio::test]
async fn graceful_manager_only_shutdown_confirms_owned_workers_exit_without_affecting_another_daemon()
 {
    let _serial = SERIAL.lock().await;
    let _subreaper = Subreaper::enter();
    let unrelated = Server::new(true).await;
    load(
        &unrelated,
        "fixture.one",
        "load unrelated daemon before graceful shutdown",
    )
    .await;
    let unrelated_pid = unrelated.model("fixture.one").await["pid"].clone();
    // Let manager cleanup run before reaping adopted workers during unwinding.
    let mut workers = Vec::new();
    let mut owned = Server::new(true).await;
    for model in ["fixture.one", "fixture.two"] {
        let loaded = load(&owned, model, "load owned daemon before graceful shutdown").await;
        let pid = loaded["pid"].as_u64().unwrap() as u32;
        assert_eq!(process(pid).unwrap().1, owned.pid());
        workers.push(OwnedWorker {
            pid: pid as libc::pid_t,
            reaped: false,
        });
    }
    assert_eq!(
        unsafe { libc::kill(owned.pid() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = owned.wait_for_manager_exit(Duration::from_secs(5)).await;
    owned.retire_reaped_manager_without_group_signal();
    assert!(status.success());
    for worker in &mut workers {
        assert!(
            process(worker.pid as u32).is_none(),
            "manager exited before confirming its owned worker was reaped"
        );
        worker.reaped = true;
    }
    assert_eq!(unrelated.model("fixture.one").await["pid"], unrelated_pid);
    let (status, result) = unrelated
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Unrelated service survives normal shutdown"}),
        )
        .await;
    assert!(status.is_success());
    assert_eq!(result["model"], "fixture.one");
    owned.start().await;
    assert_eq!(
        owned.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(owned.model("fixture.one").await["loaded"], false);
    assert_eq!(owned.model("fixture.two").await["loaded"], false);
}

#[tokio::test]
async fn manager_only_shutdown_cancels_hot_startup_and_confirms_exit_before_releasing_ownership() {
    let _serial = SERIAL.lock().await;
    let _subreaper = Subreaper::enter();
    let unrelated = Server::new(true).await;
    assert!(
        unrelated
            .post("load", json!({"model":"fixture.one"}))
            .await
            .0
            .is_success()
    );
    let unrelated_pid = unrelated.model("fixture.one").await["pid"].clone();
    // Let manager cleanup run before reaping adopted workers during unwinding.
    let mut workers = Vec::new();
    let mut owned = Server::configured(true, |config| {
        config["startup_timeout_ms"] = json!(10000);
        config["models"][0]["policy"] = json!("hot");
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--startup-delay-ms"), json!("5000")]);
    })
    .await;
    owned
        .wait_for(async || owned.event("fixture.one", "worker-startup", "started"))
        .await;
    let starting = owned.model("fixture.one").await;
    assert_eq!(starting["policy"], "hot");
    assert_eq!(starting["state"], "loading");
    let pid = starting["process_lifecycle"]["pid"].as_u64().unwrap() as u32;
    assert_eq!(starting["pid"], pid);
    assert_eq!(process(pid).unwrap().1, owned.pid());
    workers.push(OwnedWorker {
        pid: pid as libc::pid_t,
        reaped: false,
    });
    assert_eq!(
        owned.get("juiced/state").await["resources"]["reserved_bytes"],
        64
    );
    assert_eq!(
        unsafe { libc::kill(owned.pid() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = owned.wait_for_manager_exit(Duration::from_secs(5)).await;
    owned.retire_reaped_manager_without_group_signal();
    assert!(status.success());
    assert!(
        process(pid).is_none(),
        "Hot startup child survived successful manager shutdown"
    );
    workers[0].reaped = true;
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&owned.config_path).unwrap()).unwrap();
    assert_eq!(saved["models"][0]["policy"], "hot");
    assert_eq!(unrelated.model("fixture.one").await["pid"], unrelated_pid);
    let (status, result) = unrelated
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"Unrelated pipeline after Hot startup shutdown"}),
        )
        .await;
    assert!(status.is_success());
    assert_eq!(result["model"], "fixture.one");
}
