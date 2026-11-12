mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use support::Server;

type Response = (StatusCode, Value);
struct Load(tokio::task::JoinHandle<Response>);
impl Load {
    async fn finish(mut self) -> Response {
        tokio::time::timeout(Duration::from_secs(3), &mut self.0)
            .await
            .expect("owned load did not settle within three seconds")
            .unwrap()
    }
    fn finished(&self) -> bool {
        self.0.is_finished()
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn load(server: &Server, model: &str) -> Load {
    let client = server.client.clone();
    let url = format!("{}/v1/load", server.base);
    let model = model.to_owned();
    let key = server.admin_key().unwrap();
    Load(tokio::spawn(async move {
        let response = client
            .post(url)
            .bearer_auth(key)
            .json(&json!({"model":model}))
            .send()
            .await
            .expect("owned load HTTP transfer failed");
        let status = response.status();
        (
            status,
            response
                .json()
                .await
                .expect("load returned structured JSON"),
        )
    }))
}
async fn gated(pending: usize, wait_ms: u64, hot: bool) -> (Server, PathBuf) {
    let mut release = None;
    let server = Server::configured(true, |config| {
        config["admission"] =
            json!({"startup":{"concurrent":1,"pending":pending,"wait_ms":wait_ms}});
        config["startup_timeout_ms"] = json!(5000);
        config["request_timeout_ms"] = json!(5000);
        let root = std::path::Path::new(config["models"][0]["model_file"].as_str().unwrap())
            .parent()
            .unwrap()
            .to_owned();
        let path = root.join("startup-release");
        config["models"][0]["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--startup-release-file"), json!(path)]);
        if hot {
            config["models"][0]["policy"] = json!("hot");
        }
        config["models"][2]["id"] = json!("fixture.three");
        config["models"][2]["policy"] = json!("on-demand");
        config["models"][2]["args"][5] = json!(root.join("fixture.three.events"));
        release = Some(path);
    })
    .await;
    (server, release.unwrap())
}
async fn startup(server: &Server) -> Value {
    server.get("juiced/admission").await["startup"].clone()
}
async fn resources(server: &Server) -> Value {
    server.get("juiced/state").await["resources"].clone()
}
async fn wait_pending(server: &Server, count: usize) {
    server
        .wait_for(async || startup(server).await["pending"] == count)
        .await;
}
async fn queued_has_no_runtime(server: &Server, id: &str) {
    let model = server.model(id).await;
    assert_eq!(model["startup_waiting"], true);
    assert_eq!(model["loaded"], false);
    assert!(model["pid"].is_null());
    assert!(model["process_lifecycle"].is_null());
    assert!(model["runtime_identity"].is_null());
    assert!(!server.event(id, "worker-startup", "started"));
}

#[tokio::test]
async fn cold_models_queue_without_resources_and_duplicate_loads_reuse_one_startup_and_pid() {
    let (server, release) = gated(4, 5000, false).await;
    let first = load(&server, "fixture.one");
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let active = resources(&server).await;
    assert_eq!(active["reserved_bytes"], 64);
    assert_eq!(active["power"]["active"], 1);
    let duplicate = load(&server, "fixture.one");
    let second = load(&server, "fixture.two");
    wait_pending(&server, 1).await;
    let queue = startup(&server).await;
    assert_eq!(queue["settings"]["concurrent"], 1);
    assert_eq!(queue["active"], 1);
    assert_eq!(queue["active_models"], json!(["fixture.one"]));
    assert_eq!(queue["pending_models"], json!(["fixture.two"]));
    assert!(!queue["models_truncated"].as_bool().unwrap());
    queued_has_no_runtime(&server, "fixture.two").await;
    assert!(!duplicate.finished());
    let waiting = resources(&server).await;
    assert_eq!(waiting["reserved_bytes"], 64);
    assert_eq!(waiting["power"]["active"], active["power"]["active"]);
    assert_eq!(waiting["power"]["attempts"], 0);
    std::fs::write(release, b"release admitted startup").unwrap();
    let (first_status, first) = first.finish().await;
    let (duplicate_status, duplicate) = duplicate.finish().await;
    let (second_status, second) = second.finish().await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    assert_eq!(duplicate_status, StatusCode::OK, "{duplicate}");
    assert_eq!(second_status, StatusCode::OK, "{second}");
    for model in [&first, &duplicate, &second] {
        assert_eq!(model["state"], "ready");
        assert_eq!(model["loaded"], true);
    }
    assert_eq!(first["pid"], duplicate["pid"]);
    assert_eq!(
        first["process_lifecycle"]["generation"],
        duplicate["process_lifecycle"]["generation"]
    );
    assert_ne!(first["pid"], second["pid"]);
    assert_eq!(
        server.event_count("fixture.one", "worker-startup", "started"),
        1
    );
    assert_eq!(
        server.event_count("fixture.two", "worker-startup", "started"),
        1
    );
    assert_eq!(startup(&server).await["active"], 0);
    assert_eq!(startup(&server).await["pending"], 0);
    assert_eq!(resources(&server).await["reserved_bytes"], 128);
    assert_eq!(resources(&server).await["power"]["active"], 0);
}

#[tokio::test]
async fn queued_socket_cancellation_does_not_start_and_active_cancellation_releases_the_next_fifo_startup_after_exit()
 {
    let (server, _) = gated(4, 5000, false).await;
    let first = server
        .socket_request(
            "completions",
            json!({"model":"fixture.one","test_id":"cancel-active-startup"}),
        )
        .await;
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let old_pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
    let cancelled = server
        .socket_request(
            "completions",
            json!({"model":"fixture.two","test_id":"cancel-queued-startup"}),
        )
        .await;
    wait_pending(&server, 1).await;
    let third = load(&server, "fixture.three");
    wait_pending(&server, 2).await;
    assert_eq!(
        startup(&server).await["pending_models"],
        json!(["fixture.two", "fixture.three"])
    );
    queued_has_no_runtime(&server, "fixture.two").await;
    queued_has_no_runtime(&server, "fixture.three").await;
    assert_eq!(resources(&server).await["reserved_bytes"], 64);
    drop(cancelled);
    server
        .wait_for(async || {
            let model = server.model("fixture.two").await;
            startup(&server).await["pending"] == 1
                && model["startup_waiting"] == false
                && model["preparation_pending"] == false
        })
        .await;
    assert_eq!(
        startup(&server).await["pending_models"],
        json!(["fixture.three"])
    );
    let cancelled = server.model("fixture.two").await;
    assert_eq!(cancelled["startup_waiting"], false);
    assert_eq!(cancelled["preparation_pending"], false);
    assert!(cancelled["pid"].is_null());
    assert!(!server.event("fixture.two", "worker-startup", "started"));
    drop(first);
    let (status, third) = third.finish().await;
    assert_eq!(status, StatusCode::OK, "{third}");
    #[cfg(target_os = "linux")]
    assert!(
        !std::path::Path::new(&format!("/proc/{old_pid}")).exists(),
        "replacement started before the old owned child exit was confirmed"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = old_pid;
    server
        .wait_for(async || {
            server.model("fixture.one").await["loaded"] == false
                && startup(&server).await["active"] == 0
        })
        .await;
    assert_eq!(resources(&server).await["reserved_bytes"], 64);
    assert!(!server.event("fixture.two", "worker-startup", "started"));
    let (status, retried) = server.post("load", json!({"model":"fixture.two"})).await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(
        server.event_count("fixture.two", "worker-startup", "started"),
        1
    );
    assert_eq!(startup(&server).await["pending"], 0);
}

#[tokio::test]
async fn startup_queue_capacity_and_deadline_errors_leave_no_worker_or_ticket_and_allow_retry() {
    for (pending, wait_ms, status, code) in [
        (0, 5000, StatusCode::TOO_MANY_REQUESTS, "startup_queue_full"),
        (1, 100, StatusCode::GATEWAY_TIMEOUT, "startup_queue_timeout"),
    ] {
        let (server, release) = gated(pending, wait_ms, false).await;
        let first = load(&server, "fixture.one");
        server
            .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
            .await;
        let (actual, error) = server.post("load", json!({"model":"fixture.two"})).await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["error"]["code"], code);
        assert_eq!(startup(&server).await["active"], 1);
        assert_eq!(startup(&server).await["pending"], 0);
        let model = server.model("fixture.two").await;
        assert_eq!(model["startup_waiting"], false);
        assert_eq!(model["preparation_pending"], false);
        assert!(model["pid"].is_null());
        assert!(!server.event("fixture.two", "worker-startup", "started"));
        assert_eq!(resources(&server).await["reserved_bytes"], 64);
        std::fs::write(release, b"release admitted startup").unwrap();
        assert_eq!(first.finish().await.0, StatusCode::OK);
        let (actual, retried) = server.post("load", json!({"model":"fixture.two"})).await;
        assert_eq!(actual, StatusCode::OK, "{retried}");
        assert_eq!(startup(&server).await["active"], 0);
        assert_eq!(startup(&server).await["pending"], 0);
    }
}

#[tokio::test]
async fn pause_flushes_waiting_startups_keeps_admitted_work_and_resume_accepts_new_loads() {
    let (server, release) = gated(4, 5000, false).await;
    let first = load(&server, "fixture.one");
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let second = load(&server, "fixture.two");
    wait_pending(&server, 1).await;
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":true}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, error) = second.finish().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert_eq!(error["error"]["code"], "startup_paused");
    assert_eq!(startup(&server).await["paused"], true);
    assert_eq!(startup(&server).await["active"], 1);
    assert_eq!(startup(&server).await["pending"], 0);
    let (status, error) = server.post("load", json!({"model":"fixture.two"})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "admission_paused");
    assert!(!server.event("fixture.two", "worker-startup", "started"));
    std::fs::write(release, b"release admitted startup").unwrap();
    let (status, ready) = first.finish().await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(startup(&server).await["active"], 0);
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":false}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, resumed) = server.post("load", json!({"model":"fixture.two"})).await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    assert_eq!(startup(&server).await["paused"], false);
    assert_eq!(startup(&server).await["active"], 0);
}

#[tokio::test]
async fn queued_policy_revision_is_revalidated_before_resources_or_process_dispatch() {
    let (server, release) = gated(1, 5000, false).await;
    let first = load(&server, "fixture.one");
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let second = load(&server, "fixture.two");
    wait_pending(&server, 1).await;
    let current = server.model("fixture.two").await;
    let(status,disabled)=server.post("juiced/policy",json!({"model":"fixture.two","policy":"disabled","expected_revision":current["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let (status, error) = second.finish().await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "model_changed");
    assert_eq!(startup(&server).await["pending"], 0);
    assert_eq!(startup(&server).await["active"], 1);
    assert!(!first.finished());
    assert!(!release.exists());
    assert_eq!(resources(&server).await["reserved_bytes"], 64);
    assert_eq!(resources(&server).await["power"]["active"], 1);
    assert!(!server.event("fixture.two", "worker-startup", "started"));
    let model = server.model("fixture.two").await;
    assert_eq!(model["policy"], "disabled");
    assert_eq!(model["revision"], disabled["revision"]);
    assert_eq!(model["startup_waiting"], false);
    assert_eq!(model["preparation_pending"], false);
    assert!(model["pid"].is_null());
    let(status,enabled)=server.post("juiced/policy",json!({"model":"fixture.two","policy":"on-demand","expected_revision":disabled["revision"]})).await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    let retry = load(&server, "fixture.two");
    wait_pending(&server, 1).await;
    queued_has_no_runtime(&server, "fixture.two").await;
    std::fs::write(release, b"release admitted startup").unwrap();
    assert_eq!(first.finish().await.0, StatusCode::OK);
    let (status, retried) = retry.finish().await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(startup(&server).await["active"], 0);
    assert_eq!(startup(&server).await["pending"], 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn manager_shutdown_flushes_startup_queue_and_cancels_a_hot_worker_before_ready() {
    let (mut server, release) = gated(4, 5000, true).await;
    server
        .wait_for(async || server.event("fixture.one", "worker-startup", "started"))
        .await;
    let pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
    let second = load(&server, "fixture.two");
    wait_pending(&server, 1).await;
    queued_has_no_runtime(&server, "fixture.two").await;
    assert_eq!(
        unsafe { libc::kill(server.pid() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let (status, error) = second.finish().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert_eq!(error["error"]["code"], "shutting_down");
    let exited = server.wait_for_manager_exit(Duration::from_secs(5)).await;
    server.retire_reaped_manager_without_group_signal();
    assert!(exited.success());
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert!(!server.event("fixture.two", "worker-startup", "started"));
    std::fs::write(release, b"release hot startup after restart").unwrap();
    server.start().await;
    server
        .wait_for(async || {
            server.model("fixture.one").await["loaded"] == true
                && startup(&server).await["active"] == 0
                && startup(&server).await["pending"] == 0
        })
        .await;
    assert_eq!(server.model("fixture.one").await["policy"], "hot");
    assert_eq!(startup(&server).await["active"], 0);
    assert_eq!(startup(&server).await["pending"], 0);
    let (status, retried) = server.post("load", json!({"model":"fixture.two"})).await;
    assert_eq!(status, StatusCode::OK, "{retried}");
}
