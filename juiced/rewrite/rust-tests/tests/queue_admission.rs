mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{PREFIXES, Server};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

async fn server(settings: Value) -> Server {
    Server::configured(true, |config| {
        config["admission"] = settings;
        for model in config["models"].as_array_mut().unwrap() {
            model["max_inflight"] = json!(1);
        }
    })
    .await
}
async fn scheduler(server: &Server) -> Value {
    server.get("juiced/admission").await["scheduler"].clone()
}
async fn pending(server: &Server, count: u64) {
    server
        .wait_for(async || scheduler(server).await["pending"] == count)
        .await;
}
async fn active(server: &Server, count: u64) {
    server
        .wait_for(async || {
            scheduler(server).await["active"] == count
                && server.get("juiced/state").await["resources"]["active_requests"] == count
        })
        .await;
}
async fn raw(server: &Server, model: &str, id: &str, priority: &str, key: &str) -> TcpStream {
    let mut socket = TcpStream::connect(server.base.trim_start_matches("http://"))
        .await
        .unwrap();
    let body = json!({"model":model,"test_id":id,"fixture_delay_ms":2000}).to_string();
    socket.write_all(format!("POST /v1/completions HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nAuthorization: Bearer {key}\r\nX-Juiced-Priority: {priority}\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    socket
}
async fn held(server: &Server, model: &str, id: &str) -> TcpStream {
    let socket = raw(server, model, id, "interactive", server.read_key().unwrap()).await;
    server
        .wait_for(async || server.event(model, id, "started"))
        .await;
    socket
}

#[tokio::test]
async fn ordinary_queue_is_fifo_and_cancelled_waiters_never_reach_workers() {
    let server = server(json!({})).await;
    let held = held(&server, "fixture.one", "held").await;
    let first = raw(
        &server,
        "fixture.one",
        "first",
        "batch",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    let cancelled = raw(
        &server,
        "fixture.one",
        "cancelled",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 2).await;
    let second = raw(
        &server,
        "fixture.one",
        "second",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 3).await;
    drop(cancelled);
    pending(&server, 2).await;
    assert!(!server.event("fixture.one", "cancelled", "started"));
    drop(held);
    server
        .wait_for(async || server.event("fixture.one", "first", "started"))
        .await;
    pending(&server, 1).await;
    assert!(!server.event("fixture.one", "second", "started"));
    drop(first);
    server
        .wait_for(async || server.event("fixture.one", "second", "started"))
        .await;
    pending(&server, 0).await;
    drop(second);
    active(&server, 0).await;
    assert!(!server.event("fixture.one", "cancelled", "started"));
    let records = server.get("juiced/requests").await;
    assert!(records["data"].as_array().unwrap().iter().any(|record| {
        record["queue_wait_ms"]
            .as_u64()
            .is_some_and(|wait| wait > 0)
    }));
    for record in records["data"].as_array().unwrap() {
        assert!(
            record["elapsed_ms"].as_u64().unwrap() >= record["queue_wait_ms"].as_u64().unwrap()
        );
        assert!(record["completed_at_ms"].as_u64().is_some_and(|at| at > 0));
    }
}

#[tokio::test]
async fn global_and_service_pending_limits_reject_excess_without_leaking_slots() {
    for (settings, code) in [
        (
            json!({"pending_limit":1,"service_pending_limit":4}),
            "pending_limit",
        ),
        (
            json!({"pending_limit":4,"service_pending_limit":1}),
            "service_pending_limit",
        ),
    ] {
        let server = server(settings).await;
        let held = held(&server, "fixture.one", "held").await;
        let queued = raw(
            &server,
            "fixture.one",
            "queued",
            "interactive",
            server.read_key().unwrap(),
        )
        .await;
        pending(&server, 1).await;
        let (status, error) = server
            .post(
                "completions",
                json!({"model":"fixture.one","test_id":"excess"}),
            )
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error["error"]["code"], code);
        assert!(!server.event("fixture.one", "excess", "started"));
        assert_eq!(scheduler(&server).await["active"], 1);
        drop(queued);
        pending(&server, 0).await;
        drop(held);
        active(&server, 0).await;
    }
}

#[tokio::test]
async fn queue_deadline_is_structured_and_a_busy_service_does_not_block_an_independent_service() {
    let server = server(json!({"interactive_wait_ms":100})).await;
    let held = held(&server, "fixture.one", "held").await;
    let (status, error) = server
        .post(
            "completions",
            json!({"model":"fixture.one","test_id":"expired"}),
        )
        .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error["error"]["code"], "queue_timeout");
    pending(&server, 0).await;
    assert!(!server.event("fixture.one", "expired", "started"));
    assert_eq!(scheduler(&server).await["active"], 1);
    let queued = raw(
        &server,
        "fixture.one",
        "still-queued",
        "batch",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    let (status, result) = server
        .post(
            "completions",
            json!({"model":"fixture.two","test_id":"independent"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["model"], "fixture.two");
    assert!(!server.event("fixture.one", "still-queued", "started"));
    drop(queued);
    pending(&server, 0).await;
    drop(held);
    active(&server, 0).await;
}

#[tokio::test]
async fn essential_headroom_remains_available_while_ordinary_requests_queue() {
    let server = Server::configured(true, |config| {
        config["models"][0]["max_inflight"] = json!(2);
        config["models"][0]["essential_headroom"] = json!(1);
    })
    .await;
    let ordinary = held(&server, "fixture.one", "ordinary").await;
    let queued = raw(
        &server,
        "fixture.one",
        "queued",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    let essential = raw(
        &server,
        "fixture.one",
        "essential",
        "essential",
        server.admin_key().unwrap(),
    )
    .await;
    server
        .wait_for(async || server.event("fixture.one", "essential", "started"))
        .await;
    active(&server, 2).await;
    let state = scheduler(&server).await;
    let pool = state["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["service"] == "fixture.one")
        .unwrap();
    assert_eq!(pool["physical_capacity"], 2);
    assert_eq!(pool["essential_headroom"], 1);
    assert_eq!(pool["ordinary_active"], 1);
    assert_eq!(pool["pending"], 1);
    drop(essential);
    active(&server, 1).await;
    assert!(!server.event("fixture.one", "queued", "started"));
    drop(ordinary);
    server
        .wait_for(async || server.event("fixture.one", "queued", "started"))
        .await;
    drop(queued);
    pending(&server, 0).await;
    active(&server, 0).await;
}

#[tokio::test]
async fn priority_headers_require_valid_unique_values_and_essential_operator_authority() {
    let server = Server::new(true).await;
    for prefix in PREFIXES {
        for value in ["unknown", "Interactive", ""] {
            let response = server
                .client
                .post(format!("{}{prefix}/completions", server.base))
                .bearer_auth(server.read_key().unwrap())
                .header("x-juiced-priority", value)
                .json(&json!({"model":"fixture.one"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["code"],
                "invalid_priority"
            );
        }
        let response = server
            .client
            .post(format!("{}{prefix}/completions", server.base))
            .bearer_auth(server.read_key().unwrap())
            .header("x-juiced-priority", "essential")
            .json(&json!({"model":"fixture.one"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "essential_authority"
        );
        let response = server
            .client
            .post(format!("{}{prefix}/completions", server.base))
            .bearer_auth(server.admin_key().unwrap())
            .header("x-juiced-priority", "batch")
            .header("x-juiced-priority", "essential")
            .json(&json!({"model":"fixture.one"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "invalid_priority"
        );
    }
    assert_eq!(scheduler(&server).await["active"], 0);
    assert_eq!(server.model("fixture.one").await["loaded"], false);
}

#[tokio::test]
async fn maintenance_flushes_waiters_and_preserves_the_active_request_worker_and_policy() {
    let server = server(json!({})).await;
    let active_socket = held(&server, "fixture.one", "active").await;
    let mut queued = raw(
        &server,
        "fixture.one",
        "queued",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    let before = server.model("fixture.one").await;
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":true}))
            .await
            .0,
        StatusCode::OK
    );
    let mut bytes = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        queued.read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("HTTP/1.1 503"));
    assert!(text.contains("admission_paused"));
    pending(&server, 0).await;
    active(&server, 1).await;
    assert!(!server.event("fixture.one", "queued", "started"));
    assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
    assert_eq!(
        server.model("fixture.one").await["policy"],
        before["policy"]
    );
    drop(active_socket);
    active(&server, 0).await;
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":false}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, result) = server
        .post("completions", json!({"model":"fixture.one"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["model"], "fixture.one");
    assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
}

async fn expect_raw_error(mut socket: TcpStream, status: u16, code: &str) {
    let mut bytes = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        socket.read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.starts_with(&format!("HTTP/1.1 {status}")),
        "unexpected HTTP status"
    );
    assert!(text.contains(code), "missing structured error {code}");
}

#[tokio::test]
async fn policy_revision_changed_while_queued_is_revalidated_before_worker_forwarding() {
    let server = server(json!({})).await;
    let held = held(&server, "fixture.one", "held").await;
    let queued = raw(
        &server,
        "fixture.one",
        "stale-policy",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    let before = server.model("fixture.one").await;
    let (status, changed) = server
        .post(
            "juiced/policy",
            json!({"model":"fixture.one","policy":"hot","expected_revision":before["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(changed["revision"], before["revision"]);
    drop(held);
    expect_raw_error(queued, 409, "model_changed").await;
    pending(&server, 0).await;
    active(&server, 0).await;
    assert!(!server.event("fixture.one", "stale-policy", "started"));
    assert_eq!(server.model("fixture.one").await["pid"], before["pid"]);
}

#[tokio::test]
async fn removing_a_queued_service_closes_waiters_and_id_reuse_has_an_independent_pool() {
    let server = Server::configured(true, |config| {
        config["models"][0]["max_inflight"] = json!(1);
        config["models"][0]["essential_headroom"] = json!(1);
    })
    .await;
    let queued = raw(
        &server,
        "fixture.one",
        "removed-waiter",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    active(&server, 0).await;
    assert_eq!(server.model("fixture.one").await["loaded"], false);
    let current = server.model("fixture.one").await;
    let (status,disabled)=server.post("juiced/policy",json!({"model":"fixture.one","policy":"disabled","expected_revision":current["revision"]})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = server
        .post(
            "delete",
            json!({"model":"fixture.one","expected_revision":disabled["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    expect_raw_error(queued, 409, "worker_closed").await;
    pending(&server, 0).await;
    active(&server, 0).await;
    assert!(!server.event("fixture.one", "removed-waiter", "started"));
    let mut spec = server.config["models"][0].clone();
    spec["essential_headroom"] = json!(0);
    spec["policy"] = json!("disabled");
    spec["revision"] = json!(0);
    let (status, registered) = server.post("models/register", spec).await;
    assert_eq!(status, StatusCode::OK);
    let (status,_)=server.post("juiced/policy",json!({"model":"fixture.one","policy":"on-demand","expected_revision":registered["revision"]})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, result) = server
        .post(
            "completions",
            json!({"model":"fixture.one","test_id":"reused-pool"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["model"], "fixture.one");
    assert!(!server.event("fixture.one", "removed-waiter", "started"));
}

#[tokio::test]
async fn global_active_limit_bounds_execution_across_independent_worker_pools() {
    let server = server(json!({"active_limit":1})).await;
    let first = held(&server, "fixture.one", "global-active").await;
    let second = raw(
        &server,
        "fixture.two",
        "globally-queued",
        "interactive",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    active(&server, 1).await;
    assert_eq!(server.model("fixture.two").await["loaded"], false);
    assert!(!server.event("fixture.two", "globally-queued", "started"));
    drop(first);
    server
        .wait_for(async || server.event("fixture.two", "globally-queued", "started"))
        .await;
    pending(&server, 0).await;
    active(&server, 1).await;
    drop(second);
    active(&server, 0).await;
}

#[tokio::test]
async fn bounded_essential_priority_eventually_grants_an_ordinary_waiter() {
    let server = server(json!({})).await;
    let held = held(&server, "fixture.one", "held").await;
    let ordinary = raw(
        &server,
        "fixture.one",
        "ordinary-waiter",
        "batch",
        server.read_key().unwrap(),
    )
    .await;
    pending(&server, 1).await;
    let mut essential = Vec::new();
    for index in 0..9 {
        essential.push(
            raw(
                &server,
                "fixture.one",
                &format!("essential-{index}"),
                "essential",
                server.admin_key().unwrap(),
            )
            .await,
        );
        pending(&server, index + 2).await;
    }
    drop(held);
    for (index, socket) in essential.drain(..8).enumerate() {
        server
            .wait_for(async || {
                server.event("fixture.one", &format!("essential-{index}"), "started")
            })
            .await;
        assert!(!server.event("fixture.one", "ordinary-waiter", "started"));
        drop(socket);
    }
    server
        .wait_for(async || server.event("fixture.one", "ordinary-waiter", "started"))
        .await;
    assert!(!server.event("fixture.one", "essential-8", "started"));
    drop(ordinary);
    server
        .wait_for(async || server.event("fixture.one", "essential-8", "started"))
        .await;
    drop(essential);
    pending(&server, 0).await;
    active(&server, 0).await;
}
